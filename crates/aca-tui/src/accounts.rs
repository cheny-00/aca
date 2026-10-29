//! Accounts: recorded in `~/.config/aca/config.toml` —
//!
//! ```toml
//! [accounts.claude]
//! work = "~/.claude-work"
//!
//! [accounts.codex]
//! cx = "~/.codex-cx"
//! ```
//!
//! Reading them and how they're used (the config dir / env vars / args a session starts with) belong to aca-core; here are
//! the few things done on the registry, shared by the interface (the accounts page in acctpage.rs, the table in setup.rs)
//! and the command line (`aca account …`):
//! - Add one: add a line `name = "dir"` to that section; remove one: delete that line. Other content and comments are left
//!   as they are, and so are the dir and the login in it
//! - Order: Claude first, then Codex, each sorted by `[ui] menu` (the order of the `n` menu); moving one step edits that line
//! - Checking the dir: a dir that looks like the other family's (registering `~/.codex` as a Claude account) can't be
//!   added, and ones already registered like that are flagged
//! - Logged in or not: ask the tool itself (`claude auth status` / `codex login status`); aca doesn't read credentials
//!
//! Logging in goes through the tool's own command: `aca run <family>:<account> -- login`, with the environment layer this
//! account starts sessions with.

use std::path::Path;

use regex::Regex;
use toml::{Table, Value};

use aca_core::{t, tr};

use crate::data::Agent;

/// Added: the config dir (~ expanded), whether it was just created (a new one needs a login)
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

/// An account name is only letters, digits, - and _ (it's a TOML key, and must be typeable as is in a terminal)
pub fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn default_dir(a: Agent, name: &str) -> String {
    if a == Agent::Codex { format!("~/.codex-{name}") } else { format!("~/.claude-{name}") }
}

/// Run the tool's own command with this account (`aca run <family>:<account> -- args`): aca layers the environment, and
/// identity variables left by an outer session are removed too
fn run_cmd(a: Agent, name: &str, args: &str) -> String {
    let aca = aca_core::agent::self_exe().replace('\'', "'\\''");
    let spec = crate::tools::spec(fam(a).unwrap_or("claude"), name);
    if args.is_empty() { format!("'{aca}' run {spec}") } else { format!("'{aca}' run {spec} -- {args}") }
}

/// The command a new account logs in with: Claude goes to login on its first start in a new dir; Codex uses codex login
pub fn login_cmd(a: Agent, name: &str) -> String {
    run_cmd(a, name, if a == Agent::Codex { "login" } else { "" })
}

/// Log in again on an existing account (⏎ on an account in the settings page): no session, login only
pub fn relogin_cmd(a: Agent, name: &str) -> String {
    run_cmd(a, name, if a == Agent::Codex { "login" } else { "auth login" })
}

/// Expand ~ / ~/… / $HOME/…, drop a trailing /
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

/// Paths under the home dir written back as ~/…
pub(crate) fn tilde(abs: &str, home: &str) -> String {
    match abs.strip_prefix(home) {
        Some("") => "~".into(),
        Some(rest) if rest.starts_with('/') && !home.is_empty() => format!("~{rest}"),
        _ => abs.to_string(),
    }
}

/// No such file = empty
fn read(p: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        r => r,
    }
}

/// This family's recorded accounts: (name, dir as written; for a table its home, or empty if none). Unreadable / malformed
/// counts as none
fn list_in(text: &str, fam: &str) -> Vec<(String, String)> {
    let Ok(t) = text.parse::<Table>() else { return vec![] };
    let table = t.get("accounts").and_then(|v| v.get(fam)).and_then(|v| v.as_table());
    let dir = |v: &Value| v.as_str().or_else(|| v.get("home")?.as_str()).unwrap_or("").trim().to_string();
    table.into_iter().flatten().map(|(n, v)| (n.clone(), dir(v))).collect()
}

/// One account listed on the accounts page
#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub agent: Agent,
    pub name: String,
    /// Config dir, written as ~/… under the home dir; empty for an account that doesn't change dirs (env vars / args only)
    pub dir: String,
}

/// What each family is called (in table headers, and when adding an account)
pub fn who(a: Agent) -> &'static str {
    if a == Agent::Codex { "Codex" } else { "Claude Code" }
}

/// Registered accounts: Claude first, then Codex, each in `[ui] menu` order, unlisted ones following by name
pub fn all() -> Vec<Account> {
    listed(&read(&crate::config::path()).unwrap_or_default(), &home()).into_iter().map(|(a, _)| a).collect()
}

/// The full text of config.toml → the sorted accounts, each with its config dir's full path (empty for one that doesn't
/// change dirs)
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
        // Stable sort: unlisted ones keep their name order
        one.sort_by_key(|(x, _)| menu.iter().position(|n| *n == x.name).unwrap_or(usize::MAX));
        out.extend(one);
    }
    out
}

/// The one each family uses unregistered (`aca new claude` with no account): empty account name, its own dir ~/.claude /
/// ~/.codex. Counts only when installed; not when a default account is set (`[tools.<family>] account`) or its own dir is
/// already registered as an account — then that's the one listed.
/// `aca account status` and the accounts page use it to say whether it's logged in: so a new user who never registered an
/// account can tell too
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

/// Whether this program is installed: one with a / checks that file (~ expanded), otherwise look in PATH
pub(crate) fn on_path(prog: &str) -> bool {
    if prog.contains('/') {
        return Path::new(&expand(prog, &home())).is_file();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|d| d.join(prog).is_file())
}

/// Which files in a dir only one family has (history.jsonl, sessions, plugins exist in both and don't count)
const MARKS: [(Agent, &[&str]); 2] = [
    (Agent::Claude, &[".claude.json", "settings.json", "projects", "CLAUDE.md", "shell-snapshots", ".credentials.json"]),
    (Agent::Codex, &["auth.json", "config.toml", ".codex-global-state.json", "installation_id", "version.json", "session_index.jsonl"]),
];

/// Which of a family's files this dir has
fn marks(dir: &Path, a: Agent) -> Vec<&'static str> {
    let names = MARKS.iter().find(|(x, _)| *x == a).map_or(&[][..], |(_, n)| *n);
    names.iter().copied().filter(|n| dir.join(n).exists()).collect()
}

/// This dir (full path) is the other family's config dir: returns which family, and why we say so.
/// The other family's default (`~/.codex` / `~/.claude`) counts outright; for others look inside, and it counts only when
/// the other family's files outnumber this one's (once a wrong registration has started, one or two of this family's files
/// show up in it)
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

/// The sentence for one that can't be registered
fn foreign_said(a: Agent, shown: &str, (other, why): &(Agent, String)) -> String {
    tr!(
        "{shown} 是 {} 的目录({why}),不能添加为 {} 账号",
        "{shown} is a {} directory ({why}); it can't be a {} account",
        who(*other),
        who(a)
    )
}

/// Logged in or not, as the tool itself says
#[derive(Clone, Debug, PartialEq)]
pub enum Login {
    /// Logged in: how, and as whom (`max · a@b.c`, `ChatGPT`, `API key sk-…`)
    In(String),
    Out,
    /// Can't tell (tool not installed, its output not recognized): its own words
    Unknown(String),
}

/// The JSON `claude auth status` prints
fn claude_said(out: &str) -> Login {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(out.trim()) else {
        return Login::Unknown(first_line(out));
    };
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let provider = Some(s("apiProvider")).filter(|p| !p.is_empty() && p != "firstParty");
    if v.get("loggedIn").and_then(|x| x.as_bool()) != Some(true) {
        return provider.map_or(Login::Out, Login::Unknown);
    }
    // A subscription says its tier (max / pro / team), anything else says how it's logged in
    let how = [s("subscriptionType"), s("authMethod")].into_iter().find(|x| !x.is_empty());
    let email = s("email");
    // Org: a personal subscription's is just "<email>'s Organization", the same thing as the email, so not written
    let org = Some(s("orgName")).filter(|o| !o.is_empty() && (email.is_empty() || !o.contains(&email)));
    let parts: Vec<String> = [provider, how, Some(email).filter(|e| !e.is_empty()), org].into_iter().flatten().collect();
    Login::In(parts.join(" · "))
}

/// The line `codex login status` prints (the exit code isn't 0 when logged out)
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

/// Ask the tool itself whether this account is logged in: run its command with the account's environment
/// (`aca run <family>:<account> -- …`), read-only, and stop waiting after a few seconds without an answer
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
        // Codex prints this line on stderr
        Agent::Codex => codex_said(out.status.success(), if said.trim().is_empty() { &err } else { &said }),
        _ if said.trim().is_empty() => Login::Unknown(first_line(&err)),
        _ => claude_said(&said),
    }
}

/// Back up as <file name>.bak-aca first, then write a temp file and swap the whole file in; permissions as before (600
/// without a file). Read once more before swapping: if it differs from old (someone just wrote it), don't swap, return false
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

/// Remove the item keys points to, plus parent tables left empty by that: for comparing "did anything besides this item
/// change"
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

/// Where the next table header starts (none = end of file)
fn next_table(text: &str, from: usize) -> usize {
    Regex::new(r"(?m)^[ \t]*\[").unwrap().find_at(text, from).map_or(text.len(), |m| m.start())
}

/// The newlines to add before appending a section at the end: one blank line after what's there
fn gap(text: &str) -> &'static str {
    if text.is_empty() || text.ends_with("\n\n") {
        ""
    } else if text.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    }
}

/// Add a line `name = "dir"` under `[accounts.<family>]` in config.toml: after the section's last key if it exists,
/// otherwise as a new section at the end; other content (comments, other settings) left as is
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

/// The checks before adding, without writing: are name and dir valid, do they duplicate existing ones, can config.toml be
/// edited. Returns (config.toml's current full text, the full text after adding, the expanded dir)
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
    // Within one family, neither the name nor the dir may duplicate an existing one
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

/// Check only, no write (the `aca setup` table lists it first, and everything is done together on ⏎): returns the expanded
/// dir. The real write is [`add_in`]: recorded in the account table, the dir created if missing
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

/// Remove the `name = …` line under `[accounts.<family>]` in config.toml, the rest as is. Other styles (a
/// `[accounts.<family>.<name>]` table, a value spanning several lines) aren't recognized and aren't changed
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

/// Where the `name = …` line under `[accounts.<family>]` is (including its trailing newline). No such section or line =
/// None
fn account_line(text: &str, fam: &str, name: &str) -> Option<std::ops::Range<usize>> {
    let header = Regex::new(&format!(r"(?m)^[ \t]*\[[ \t]*accounts[ \t]*\.[ \t]*{fam}[ \t]*\][ \t]*(?:#[^\n]*)?(?:\r?\n|\z)")).unwrap();
    let h = header.find(text)?;
    let end = next_table(text, h.end());
    let key = Regex::new(&format!(r#"(?m)^[ \t]*["']?{}["']?[ \t]*=[^\n]*(?:\n|\z)"#, regex::escape(name))).unwrap();
    let line = key.find(&text[h.end()..end])?;
    Some(h.end() + line.start()..h.end() + line.end())
}

/// An account's value written as one line: dir only as `"dir"`, anything else as `{ home = …, args = […] }`, dir first and
/// args last
fn inline(v: &Value) -> String {
    let Value::Table(t) = v else { return v.to_string() };
    let first = ["home", "env", "env_file"];
    let mut keys: Vec<&String> = t.keys().filter(|k| *k != "args").collect();
    keys.sort_by_key(|k| first.iter().position(|f| f == k).unwrap_or(first.len()));
    keys.extend(t.keys().filter(|k| *k == "args"));
    let parts: Vec<String> = keys.into_iter().map(|k| format!("{k} = {}", inline(&t[k.as_str()]))).collect();
    if parts.is_empty() { "{}".into() } else { format!("{{ {} }}", parts.join(", ")) }
}

/// Replace one account's own launch args (`args`) in config.toml. `name = "dir"` with args added becomes
/// `name = { home = "dir", args = […] }`, and once the args are cleared, one left with only a dir goes back to
/// `name = "dir"`; a comment after that line stays. One written as a table (`[accounts.<family>.<name>]`) gets its args line
/// changed. Not a character else changes; unrecognized styles, like a value spanning several lines, aren't changed
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
    // What it should look like after the change
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
            // Where the value ends: with a comment after it, the part before # alone parses as the original value
            let rest = row[eq + 1..].trim_end_matches(['\n', '\r']);
            let same = |v: &str| format!("x = {v}").parse::<Table>().is_ok_and(|t| t.get("x") == Some(&old));
            let cut = rest.char_indices().filter(|(_, c)| *c == '#').map(|(i, _)| i).chain([rest.len()]).find(|i| same(&rest[..*i]));
            let cut = cut.ok_or_else(odd)?;
            let gap = &rest[rest[..cut].trim_end().len()..cut];
            let tail = &row[eq + 1 + rest.len()..];
            format!("{}{} {}{gap}{}{tail}{}", &text[..line.start], &row[..=eq], inline(&want), &rest[cut..], &text[line.end..])
        }
        // Written as a table: change the args line under it
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

/// Replace one account's own launch args (the previous file saved as config.toml.bak-aca). false = same as before, not
/// written
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

/// Replace the launch args every account of this family gets (`[tools.<family>] args`) in config.toml: a comment after the
/// line stays (as on an account line); without the line it's added under the header, without the section it's added at the
/// end; not a character else changes. Unrecognized styles aren't changed
fn with_common_args(text: &str, fam: &str, args: &[String]) -> Result<String, String> {
    let list = Value::Array(args.iter().cloned().map(Value::String).collect());
    let new = crate::config::with_value(text, &format!("tools.{fam}"), "args", &list).ok_or(t!(
        "无法识别 config.toml 的写法,未修改(请手动修改)",
        "can't make out how config.toml is written; nothing changed (edit those lines yourself)"
    ))?;
    // with_value replaces that line in place: with the same line count, the line that differs is it, so re-attach the
    // original line's comment
    let (old, got): (Vec<&str>, Vec<&str>) = (text.lines().collect(), new.lines().collect());
    let Some(i) = (old.len() == got.len()).then(|| (0..old.len()).find(|&i| old[i] != got[i])).flatten() else { return Ok(new) };
    let rest = old[i].split_once('=').map_or("", |(_, r)| r);
    // A `#` inside a string doesn't count: the part up to it must parse as a value, and only what follows is the comment
    let cut = rest.match_indices('#').map(|(j, _)| j).find(|&j| format!("x = {}", &rest[..j]).parse::<Table>().is_ok());
    let Some(cut) = cut else { return Ok(new) };
    let mut lines: Vec<String> = got.iter().map(|l| l.to_string()).collect();
    lines[i].push_str(&rest[rest[..cut].trim_end().len()..]);
    let out = lines.join("\n") + "\n";
    Ok(if out.parse::<Table>().ok() == new.parse::<Table>().ok() { out } else { new })
}

/// Replace the launch args every account of this family gets (the previous file saved as config.toml.bak-aca). false =
/// same as before, not written
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

/// The dir the removed account had (written as ~/… under the home dir; empty for one that doesn't change dirs)
#[derive(Debug, PartialEq)]
pub struct Removed {
    pub dir: String,
}

/// Remove an account: only drop that line from config.toml (the previous file saved as config.toml.bak-aca). The config
/// dir and the login in it are untouched
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

/// Which tools use it when no account is given (`[tools.<name>] account`): removing it leaves those tools unable to start
pub(crate) fn default_of(cfg: &aca_core::config::Config, a: Agent, name: &str) -> Vec<String> {
    let fam = fam(a).unwrap_or_default();
    cfg.tools.keys().filter(|t| cfg.tool(t).is_some_and(|d| d.kind == fam && d.account == name)).cloned().collect()
}

/// Move this account one step up / down within its family; false if it's already at the top / bottom.
/// The order lives in `[ui] menu`: it's the `n` menu's order, one and the same. Moving writes out the whole menu order
/// (ones added later still follow at the end)
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
    // The menu's whole current order (hidden ones included): take only this one out and insert it before / after its
    // neighbor in the same family, nobody else moves (the two families are interleaved in the menu: with a swap, the one
    // jumped over would move elsewhere too)
    let lay = crate::tools::Layout { menu, hide: vec![] };
    let mut names: Vec<String> = vec![];
    for t in crate::tools::menu(&cfg, &lay).into_iter().filter(|t| t.tool != "shell" && t.name != name) {
        if !names.contains(&t.name) {
            names.push(t.name);
        }
    }
    let Some(j) = names.iter().position(|n| *n == by) else { return Ok(false) };
    names.insert(if up { j } else { j + 1 }, name.to_string());
    // With same-named accounts in both families the menu can't tell them apart by name: if the result isn't the wanted
    // order, don't write
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
        // No order given: each family by name; an account that doesn't change dirs has an empty dir
        assert_eq!(
            got(text),
            [
                row(Agent::Claude, "bed", "", ""),
                row(Agent::Claude, "work", "~/.claude-work", "/h/.claude-work"),
                row(Agent::Codex, "ax", "~/.ax", "/h/.ax"),
                row(Agent::Codex, "cx", "~/.codex-cx", "/h/.codex-cx"),
            ]
        );
        // Those in [ui] menu go first (the same order as the n menu), the two families still kept apart
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
        // The org is written only when it isn't the same thing as the email
        let team = r#"{"loggedIn": true, "email": "me@example.com", "orgName": "Acme", "subscriptionType": "team"}"#;
        assert_eq!(claude_said(team), Login::In("team · me@example.com · Acme".into()));
        let own = r#"{"loggedIn": true, "email": "me@example.com", "orgName": "me@example.com's Organization", "subscriptionType": "max"}"#;
        assert_eq!(claude_said(own), Login::In("max · me@example.com".into()));
        assert_eq!(
            claude_said("{\n  \"loggedIn\": false,\n  \"authMethod\": \"none\",\n  \"apiProvider\": \"firstParty\"\n}\n"),
            Login::Out
        );
        // Not using its own login (Bedrock…): says which provider, doesn't claim logged in or out
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
        // Started once after a wrong registration, so it gained one file of this family: still the other family's
        std::fs::write(d.join(".cx/.claude.json"), "{}").unwrap();
        assert_eq!(who(Agent::Claude, &codex).map(|x| x.0), Some(Agent::Codex));
        // The other family's default dir: counts even before it exists; an empty or new dir is nobody's
        assert_eq!(who(Agent::Claude, &format!("{home}/.codex")), Some((Agent::Codex, "Codex 的默认目录".into())));
        assert_eq!(who(Agent::Codex, &format!("{home}/.claude")), Some((Agent::Claude, "Claude Code 的默认目录".into())));
        assert_eq!((who(Agent::Claude, &make(".empty", &[])), who(Agent::Codex, &format!("{home}/.nowhere"))), (None, None));
        // Stopped right when adding: config.toml untouched
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
        // A one-line dir: becomes the form with args, comment kept
        let out = with_args(old, "claude", "work", &args(&["--model", "sonnet"])).unwrap();
        assert_eq!(
            out,
            old.replace(
                "work = \"~/.claude-work\" # 公司",
                "work = { home = \"~/.claude-work\", args = [\"--model\", \"sonnet\"] } # 公司"
            )
        );
        // Cleared: written back as a one-line dir
        assert_eq!(with_args(&out, "claude", "work", &[]).unwrap(), old);
        // Already a small inline table: added at the end, env kept; still a table after clearing
        let out = with_args(old, "claude", "me", &args(&["a b"])).unwrap();
        assert!(out.contains("me = { home = \"~/.claude-me\", env = { X = \"1\" }, args = [\"a b\"] }\n"), "{out}");
        assert_eq!(with_args(&out, "claude", "me", &[]).unwrap(), old);
        // The same-named account of the other family is unaffected; same as before = unchanged
        let out = with_args(old, "codex", "work", &args(&["--yolo"])).unwrap();
        assert!(
            out.contains("[accounts.codex]\nwork = { home = \"~/.codex-work\", args = [\"--yolo\"] }\n")
                && out.contains("work = \"~/.claude-work\" # 公司")
        );
        assert_eq!(with_args(old, "codex", "work", &[]).unwrap(), old);
        // Written as a table: its args line changes
        let table = "[accounts.codex.labs]\nhome = \"~/.labs\"\n";
        assert_eq!(
            with_args(table, "codex", "labs", &args(&["-p", "x"])).unwrap(),
            "[accounts.codex.labs]\nargs = [\"-p\", \"x\"]\nhome = \"~/.labs\"\n"
        );
        // A value spanning several lines, no such account, a broken file: unchanged
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
        // Cleared: the empty line stays; a # inside a string isn't taken as a comment
        let out = with_common_args(&out, "claude", &[]).unwrap();
        assert_eq!(out, old.replace("args = [\"--model\", \"opus\"]", "args = []"));
        let quoted = "[tools.claude]\nargs = [\"#x\"]\n";
        assert_eq!(with_common_args(quoted, "claude", &args(&["y"])).unwrap(), "[tools.claude]\nargs = [\"y\"]\n");
        // No such line: added under the header; no such section: added at the end
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
        // It was the only one left in the section: the header stays (an empty section does no harm)
        assert_eq!(drop_line(old, "codex", "work").unwrap(), old.replace("work = \"~/.codex-work\"\n", ""));
        // Written as a table, a value spanning several lines, a broken file: not recognized, unchanged
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
        // Which tools use it by default: to be said before removing
        let cfg = aca_core::config::parse(old);
        assert_eq!((default_of(&cfg, Agent::Claude, "work"), default_of(&cfg, Agent::Codex, "work")), (vec!["claude".to_string()], vec![]));
        assert_eq!(remove_in(&path, &home, Agent::Claude, "work").unwrap(), Removed { dir: "~/.claude-work".into() });
        assert_eq!(remove_in(&path, &home, Agent::Claude, "bed").unwrap(), Removed { dir: String::new() });
        let now = std::fs::read_to_string(&path).unwrap();
        assert_eq!(now, old.replace("work = \"~/.claude-work\" # 公司\n", "").replace("bed = { env = { X = \"1\" } }\n", ""));
        // The dir is untouched; the other family's same-named account is still there
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
        // Up from the top, down from the bottom: no move, no write
        assert_eq!(shift_in(&path, &home, Agent::Claude, "sub", true), Ok(false));
        assert_eq!(shift_in(&path, &home, Agent::Codex, "corp", false), Ok(false));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
        // The hidden one can move too; the whole menu order is written out (named tools included), other lines unchanged
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
        // The two families are interleaved in the menu: only the moved one changes place; the ones passed (firm, corp)
        // keep their order relative to the other family's
        let menu = || crate::config::list_in(&std::fs::read_to_string(&path).unwrap(), "ui", "menu");
        assert_eq!(menu(), ["sub", "official", "firm", "corp", "labs", "ccx"]);
        assert_eq!(shift_in(&path, &home, Agent::Codex, "labs", true), Ok(true));
        assert_eq!(shift_in(&path, &home, Agent::Claude, "sub", false), Ok(true));
        assert_eq!(menu(), ["official", "sub", "firm", "labs", "corp", "ccx"]);
        assert!(shift_in(&path, &home, Agent::Codex, "nope", true).is_err());
        // [ui] menu written over several lines: can't edit it, says where to order it by hand
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
        // The section is empty: right after the header
        let t = add_line(old, "codex", "cx", "~/.codex-cx").unwrap();
        assert_eq!(t, format!("{old}cx = \"~/.codex-cx\"\n"));
        // No such section: a new one at the end
        assert_eq!(add_line("x = 1", "claude", "a", "~/a").unwrap(), "x = 1\n\n[accounts.claude]\na = \"~/a\"\n");
        assert_eq!(add_line("", "codex", "a", "~/a").unwrap(), "[accounts.codex]\na = \"~/a\"\n");
        assert_eq!(aca_core::config::parse(&t).accounts["codex"]["cx"].home, aca_core::paths::expand("~/.codex-cx"));
        // An unrecognized style, a broken file: unchanged
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
        // Same name, same dir, invalid name: not added, file untouched
        assert_eq!(add_in(&path, &home, Agent::Codex, "cx", "~/.other").unwrap_err(), "账号已存在:cx");
        assert_eq!(add_in(&path, &home, Agent::Claude, "t2", "~/.ct/").unwrap_err(), "该目录已属于账号 team");
        assert_eq!(add_in(&path, &home, Agent::Codex, "c2", "~/.codex-cx").unwrap_err(), "该目录已属于账号 cx");
        assert!(add_in(&path, &home, Agent::Claude, "a b", "~/.x").is_err());
        assert!(add_in(&path, &home, Agent::Claude, "rel", "rel/dir").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), a);
        // Check only: says the same sentence, returns the expanded dir when it can be added, touches neither file nor dir
        assert_eq!(check_in(&path, &home, Agent::Codex, "cx", "~/.other").unwrap_err(), "账号已存在:cx");
        assert_eq!(check_in(&path, &home, Agent::Claude, "me", "~/.claude-me/").unwrap(), format!("{home}/.claude-me"));
        assert!(!d.join(".claude-me").exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), a);
        // Same name but the other family: allowed; the dir already exists, so no login needed
        std::fs::create_dir_all(d.join(".claude-cx")).unwrap();
        assert!(!add_in(&path, &home, Agent::Claude, "cx", "~/.claude-cx").unwrap().created);
        let _ = std::fs::remove_dir_all(&d);
    }
}
