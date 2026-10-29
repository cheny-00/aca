//! `~/.config/aca/config.toml`: accounts and tools. All optional; it works with nothing written at all (claude / codex use their
//! own default configuration directories).
//!
//! ```toml
//! [accounts.claude]            # account = the extra things added when starting it, in the tool's own terms.
//!                              # A string just changes the configuration directory
//! work = "~/.claude-work"
//! personal = "~/.claude-personal"
//! bedrock = { env = { CLAUDE_CODE_USE_BEDROCK = "1", AWS_PROFILE = "work" } }
//!
//! [accounts.codex]
//! work = "~/.codex-work"
//! team = { home = "~/.codex2", args = ["--profile", "team"] } # arguments on top of the directory
//! api = { env_file = "~/.secrets/openai.env" }                # same directory, different key
//!
//! [tools.claude]               # defaults for a built-in tool
//! account = "work"             # the account used when none is named
//! args = ["--dangerously-skip-permissions"]
//! mod = true                   # start it with aca's mod: limits without the status bar
//!                              # (off when unset; the API is early, see agent/claude_mod.rs)
//!
//! [tools.ccx]                  # a tool you name yourself: `aca new ccx`
//! kind = "claude"              # which built-in tool it starts as and takes hooks from (claude / codex / pi / opencode / shell)
//! cmd = ["claude"]             # unset = that tool's command
//! args = ["--model", "opus"]
//! account = "personal"
//! env = { ANTHROPIC_BASE_URL = "https://…" }   # environment variables added at start; any kind of tool can have them
//!
//! [tools.codex]
//! hook_trust = "bypass"        # unset: Codex asks "Hooks need review" once per account, and stops asking after "Trust all"
//!
//! [inbox]                      # passing messages between sessions
//! notify = ["done", "failed"]  # which events a worker tells the session that delegated it about; all = every one, unset = none
//!
//! [trust]
//! folder = true                # answer "do you trust this folder?" for you when a session starts (Claude and Codex both ask the
//!                              # first time in a folder); unset = don't answer for you
//!
//! [idle]                       # idle sessions
//! stop_after = "3h"            # stop after idle this long (the agent exits, the pane stays, marked ☾ in the list;
//!                              # ⏎ / aca start resumes). Write 30m, 3h, 1d, 1h30m; never or unset = no auto-stop.
//!                              # Checked every 5 minutes while the aca interface is open; not stopped: mid-turn,
//!                              # someone attached, background tasks running, etc. (see session::stop_idle)
//! remove_after = "30d"         # delete after stopped (■ ☾, archived ones too) and untouched this long; into the trash
//!                              # like d, b brings it back within a day. never or unset = keep. Pinned ones and ones
//!                              # with a note are kept (see aca-tui's `App::stale`)
//!
//! [keys]                       # keys aca binds in tmux itself (no prefix, only caught in aca's sessions;
//!                              # nothing written to your tmux.conf)
//! next = "C-M-n"               # go to the next session that needs you (`aca next`). This is the default; "" = unbound
//!
//! [login]                      # logins aca starts (⏎ on the accounts page, aca run … login): no default browser
//!                              # opened for you, the link pops up to pick from
//! browsers = ["Firefox", { name = "Chrome 工作", cmd = ["google-chrome", "--profile-directory=Profile 1"] }]
//!                              # unset: found automatically (.app on Mac, .desktop on Linux). A string: the App's
//!                              # name on Mac, a command on Linux; a table: name + command, the link goes last
//!                              # (or wherever {url} is in the arguments)
//! remote = "auto"              # whether to act as remote (no browsers listed, a login finishable on another device):
//!                              # auto looks at SSH and whether Linux has a GUI; yes / no fixes it
//!
//! [tools.aider]                # no kind and not a built-in name: a declared tool; the keys below tell aca
//!                              # how to treat it (see agent/declared.rs)
//! cmd = ["aider"]              # unset = the tool name
//! home_var = "AIDER_HOME"      # which environment variable gets the account's home; unset = accounts can only use env / args
//! resume = ["--session", "{conv}"]   # how to start it resuming a conversation; {conv} becomes the conversation id
//!                                    # (the hook reports the id)
//! fork = ["--fork", "{conv}"]
//! busy = ['Working\.\.\.']     # running when this shows on the screen (regex)
//! ask = ['\(y/n\)']            # needs you when this shows on the screen (regex)
//! title = "pane"               # the title comes from the pane title; unset = only what the hook reports
//! ```

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{agent, paths};

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Tool {
    /// Which built-in tool; empty = a declared tool
    pub kind: String,
    pub cmd: Vec<String>,
    pub args: Vec<String>,
    pub account: String,
    pub env: BTreeMap<String, String>,
    /// Codex: `"bypass"` = add `--dangerously-bypass-hook-trust` at start, so it doesn't ask "Hooks need review"
    pub hook_trust: String,
    /// Claude: `true` = start it with aca's mod (`--plugin-dir`); limits come from the mod, not the status bar. See agent/claude_mod.rs
    #[serde(rename = "mod")]
    pub mods: bool,
    // The keys below are only for declared tools; the built-in tools each have their own way and ignore them
    pub home_var: String,
    pub resume: Vec<String>,
    pub fork: Vec<String>,
    pub busy: Vec<String>,
    pub ask: Vec<String>,
    pub title: String,
    /// The plugin directories for this tool from the enabled aca plugins, passed at this start (see [`crate::plugin::agent_dirs`]). Filled
    /// at start, not written in the config file
    #[serde(skip)]
    pub plugin_dirs: Vec<String>,
    /// MCP servers mounted directly for this start (see [`crate::plugin::mount`]): (name, arguments handed to aca). Filled at start
    #[serde(skip)]
    pub mcp: Vec<(String, Vec<String>)>,
    /// Codex: aca plugins to enable this time (`-c plugins.<name>@aca.enabled=true`). Filled at start
    #[serde(skip)]
    pub codex_plugins: Vec<String>,
}

/// Account: one more layer on top of the tool when starting an agent. Authentication is the harness's own business (its config
/// directory, its login, its key); aca just passes these along as they are every time, without knowing or caring whether it's a
/// subscription, an API key or a Codex profile
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(from = "AccountDef")]
pub struct Account {
    /// Configuration directory (full path): passed in the environment variable this tool reads (`CLAUDE_CONFIG_DIR` / `CODEX_HOME` /
    /// a declared tool's `home_var`)
    pub home: String,
    pub env: BTreeMap<String, String>,
    /// A file with one `KEY=value` per line, read fresh at each start (where the key lives; aca doesn't store its contents)
    pub env_file: String,
    /// Arguments added every time, such as Codex's `--profile team`
    pub args: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AccountDef {
    /// Shorthand: only changes the configuration directory
    Dir(String),
    Layer(Layer),
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Layer {
    home: String,
    env: BTreeMap<String, String>,
    env_file: String,
    args: Vec<String>,
}

impl From<AccountDef> for Account {
    fn from(d: AccountDef) -> Account {
        match d {
            AccountDef::Dir(home) => Account { home: dir(&home), ..Default::default() },
            AccountDef::Layer(l) => Account { home: dir(&l.home), env: l.env, env_file: paths::expand(l.env_file.trim()), args: l.args },
        }
    }
}

/// A directory written in the config: ~ expanded, trailing / removed
pub(crate) fn dir(p: &str) -> String {
    let d = paths::expand(p.trim());
    if d.len() > 1 { d.trim_end_matches('/').to_string() } else { d }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// tool → account name → that layer
    pub accounts: BTreeMap<String, BTreeMap<String, Account>>,
    pub tools: BTreeMap<String, Tool>,
    pub inbox: Inbox,
    pub trust: Trust,
    pub keys: Keys,
    #[serde(deserialize_with = "lenient")]
    pub login: Login,
    #[serde(deserialize_with = "lenient")]
    pub idle: Idle,
}

/// `[idle]`: idle sessions (see [`crate::session::stop_idle`])
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Idle {
    /// Stop after idle this long; kept as written, read by [`Idle::stop_after`] when used
    pub stop_after: Option<toml::Value>,
    /// Delete after stopped and untouched this long (into the trash); kept as written, read by [`Idle::remove_after`] when used
    pub remove_after: Option<toml::Value>,
}

impl Idle {
    /// Seconds idle before stopping; no auto-stop = `Ok(None)`, unreadable = Err saying why (nothing is stopped then either)
    pub fn stop_after(&self) -> Result<Option<u64>, String> {
        after("stop_after", "3h", &self.stop_after)
    }

    /// Seconds stopped and untouched before deleting; never = `Ok(None)`, unreadable = Err saying why (nothing is deleted then either)
    pub fn remove_after(&self) -> Result<Option<u64>, String> {
        after("remove_after", "30d", &self.remove_after)
    }

    /// The first of the two that's written wrong
    fn problem(&self) -> Option<String> {
        self.stop_after().err().or_else(|| self.remove_after().err())
    }
}

/// One duration in `[idle]`: never, empty, 0 or unset = don't (None); eg is the example given when it's wrong
fn after(name: &str, eg: &str, v: &Option<toml::Value>) -> Result<Option<u64>, String> {
    match v {
        None => Ok(None),
        Some(toml::Value::String(s)) if s.trim().eq_ignore_ascii_case("never") => Ok(None),
        Some(toml::Value::String(s)) => span(s).map(|n| (n > 0).then_some(n)).ok_or_else(|| {
            crate::tr!(
                "{name} 应写为 30m、3h、1d、1h30m 或 never,无法识别 \"{s}\"",
                "write {name} like 30m, 3h, 1d, 1h30m or never, not \"{s}\""
            )
        }),
        Some(v) => Err(crate::tr!(
            "{name} 应为带单位的字符串,如 \"{eg}\",无法识别 {v}",
            "{name} takes a string with a unit, like \"{eg}\", not {v}"
        )),
    }
}

/// Duration: one or more "number + unit" written together, units s / m / h / d (`90m`, `1h30m`). Empty = 0; unreadable = None
pub fn span(text: &str) -> Option<u64> {
    let (mut total, mut num) = (0u64, None::<u64>);
    for c in text.trim().to_ascii_lowercase().chars() {
        match c {
            '0'..='9' => num = Some(num.unwrap_or(0).checked_mul(10)?.checked_add(c as u64 - '0' as u64)?),
            's' | 'm' | 'h' | 'd' => {
                let unit = match c {
                    's' => 1,
                    'm' => 60,
                    'h' => 3600,
                    _ => 86400,
                };
                total = total.checked_add(num.take()?.checked_mul(unit)?)?;
            }
            _ => return None,
        }
    }
    // The last number has no unit
    num.is_none().then_some(total)
}

/// `[login]`: logins aca starts, and how the link reaches you (see `agent::quiet_login` and the interface's browser.rs)
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Login {
    /// Which browsers to list when the link pops up; unset = found automatically, set = only these
    pub browsers: Vec<Browser>,
    pub remote: Remote,
}

/// One browser: the name shown in the table, and the command that opens the link (the link goes last; or wherever `{url}` is in
/// the arguments)
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(from = "BrowserDef")]
pub struct Browser {
    pub name: String,
    pub cmd: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BrowserDef {
    /// Shorthand: the App's name on Mac (`open -a`), a command elsewhere
    Short(String),
    Full {
        #[serde(default)]
        name: String,
        cmd: Vec<String>,
    },
}

impl From<BrowserDef> for Browser {
    fn from(d: BrowserDef) -> Browser {
        // When it's a path, the table only shows the file name (`/opt/x/firefox` → firefox, `/Applications/Arc.app` → Arc)
        let base = |s: &str| s.trim_end_matches('/').rsplit('/').next().unwrap_or(s).trim_end_matches(".app").to_string();
        match d {
            BrowserDef::Short(s) if cfg!(target_os = "macos") => Browser { name: base(&s), cmd: vec!["open".into(), "-a".into(), s] },
            BrowserDef::Short(s) => Browser { name: base(&s), cmd: vec![paths::expand(&s)] },
            BrowserDef::Full { name, mut cmd } => {
                if let Some(c) = cmd.first_mut() {
                    *c = paths::expand(c);
                }
                let name = if name.trim().is_empty() { cmd.first().map(|c| base(c)).unwrap_or_default() } else { name };
                Browser { name, cmd }
            }
        }
    }
}

/// `[login] remote`: whether to act as remote (see `agent::remote`). `"auto"` (unset) / `"yes"` / `"no"`; true / false work too
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(try_from = "toml::Value")]
pub enum Remote {
    #[default]
    Auto,
    Yes,
    No,
}

impl TryFrom<toml::Value> for Remote {
    type Error = String;
    fn try_from(v: toml::Value) -> Result<Remote, String> {
        match &v {
            toml::Value::Boolean(b) => return Ok(if *b { Remote::Yes } else { Remote::No }),
            toml::Value::String(s) => match s.trim() {
                "" | "auto" => return Ok(Remote::Auto),
                "yes" | "true" => return Ok(Remote::Yes),
                "no" | "false" => return Ok(Remote::No),
                _ => {}
            },
            _ => {}
        }
        Err(crate::tr!("remote 只能为 auto / yes / no,无法识别 {v}", "remote takes auto / yes / no, not {v}"))
    }
}

/// A wrong `[login]` or `[idle]` counts as unset without dragging the rest down (only a broken file as a whole counts as empty);
/// [`login_problem`] and [`idle_problem_in`] say what's wrong
fn lenient<'de, D: serde::Deserializer<'de>, T: serde::de::DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    Ok(toml::Value::deserialize(d)?.try_into().unwrap_or_default())
}

/// The key when `[keys] next` is unset: Ctrl+Alt+N
pub const NEXT_KEY: &str = "C-M-n";

/// `[keys]`: keys aca binds in tmux itself, the same way as Ctrl+Q and plugin hotkeys (see [`crate::tmux::bind_hotkeys`])
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Keys {
    /// Go to the next session that needs you (`aca next`): a tmux key name. Unset = [`NEXT_KEY`], `""` = unbound
    pub next: Option<String>,
}

impl Keys {
    /// Which key to bind; none = `Ok(None)`. One that can't be a hotkey isn't bound, and Err says why
    pub fn next_key(&self) -> Result<Option<String>, String> {
        let k = self.next.as_deref().unwrap_or(NEXT_KEY).trim();
        if k.is_empty() {
            return Ok(None);
        }
        crate::plugin::hotkey_ok(k).map(|_| Some(k.to_string()))
    }
}

/// `[trust]`: the "do you trust …" questions agents ask when they start
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Trust {
    /// `true` = after a session starts, when "do you trust this folder?" shows on the screen, choose trust for you (see [`crate::trust`]).
    /// The folder's own configuration (hooks, MCP servers) then runs without asking: only turn it on where you pick the folders yourself
    pub folder: bool,
}

/// `[inbox]`: passing messages between sessions
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Inbox {
    /// Which events a worker tells the session that delegated it about: done (finished a turn) / failed (error) / exited / waiting
    /// (waiting for a confirmation). `all` = all four; unset = none (no word, nobody woken); `aca new --notify` sets it per session
    pub notify: Vec<String>,
}

pub fn load() -> Config {
    std::fs::read_to_string(paths::config_dir().join("config.toml")).map(|t| parse(&t)).unwrap_or_default()
}

/// If config.toml is broken, where. [`load`] stays quiet (hooks mustn't fail because of it); the CLI uses this to mention it
pub fn problem() -> Option<String> {
    let text = std::fs::read_to_string(paths::config_dir().join("config.toml")).ok()?;
    toml::from_str::<Config>(&text).err().map(|e| e.to_string())
}

/// If `[login]` is wrong, what's wrong: that section alone counts as unset (see [`lenient`]), the rest is used as usual
pub fn login_problem() -> Option<String> {
    login_problem_in(&std::fs::read_to_string(paths::config_dir().join("config.toml")).ok()?)
}

pub fn login_problem_in(text: &str) -> Option<String> {
    let login = toml::from_str::<toml::Value>(text).ok()?.get("login")?.clone();
    login.try_into::<Login>().err().map(|e| e.to_string().trim().to_string())
}

/// If `[idle]` is wrong, what's wrong (no auto-stop then)
pub fn idle_problem() -> Option<String> {
    idle_problem_in(&std::fs::read_to_string(paths::config_dir().join("config.toml")).ok()?)
}

pub fn idle_problem_in(text: &str) -> Option<String> {
    let idle = toml::from_str::<toml::Value>(text).ok()?.get("idle")?.clone();
    match idle.try_into::<Idle>() {
        Ok(i) => i.problem(),
        Err(e) => Some(e.to_string().trim().to_string()),
    }
}

/// A broken config counts as empty: neither creating sessions nor hooks should fail because of it
pub fn parse(text: &str) -> Config {
    toml::from_str(text).unwrap_or_default()
}

impl Config {
    /// The definition for a tool name; the built-in tools have one even when unset. A built-in name with no kind gets itself as kind;
    /// other names with no kind are declared tools, and kind stays empty
    pub fn tool(&self, name: &str) -> Option<Tool> {
        let builtin = agent::builtin(name).is_some();
        let mut t = self.tools.get(name).cloned().or_else(|| builtin.then(Tool::default))?;
        if t.kind.is_empty() && builtin {
            t.kind = name.to_string();
        }
        Some(t)
    }

    /// A registered account; none = None
    pub fn account(&self, kind: &str, name: &str) -> Option<&Account> {
        self.accounts.get(kind)?.get(name)
    }

    /// The names the `n` menu / `aca new` accept: tools, and "tool:account"
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = agent::kinds().map(String::from).collect();
        v.extend(self.tools.keys().filter(|k| agent::builtin(k).is_none()).cloned());
        for (kind, accounts) in &self.accounts {
            v.extend(accounts.keys().map(|a| format!("{kind}:{a}")));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_tools_exist_without_any_config_and_custom_ones_name_their_kind() {
        let c = parse("");
        assert_eq!(c.tool("claude").unwrap().kind, "claude");
        assert!(c.tool("ccx").is_none());
        let c = parse(
            "[accounts.claude]\nsub = \"~/.claude-subscription/\"\n[tools.claude]\naccount = \"sub\"\n\
             [tools.ccx]\nkind = \"claude\"\nargs = [\"--model\", \"opus\"]\n",
        );
        assert_eq!(c.tool("claude").unwrap().account, "sub");
        assert_eq!(
            c.tool("ccx").unwrap(),
            Tool { kind: "claude".into(), args: vec!["--model".into(), "opus".into()], ..Default::default() }
        );
        assert!(c.account("claude", "sub").unwrap().home.ends_with("/.claude-subscription"));
        assert!(c.account("claude", "nope").is_none());
        assert_eq!(c.names(), ["claude", "codex", "pi", "opencode", "shell", "ccx", "claude:sub"]);
        // A broken file doesn't blow up
        assert_eq!(parse("[accounts\nx"), Config::default());
    }

    #[test]
    fn the_next_key_has_a_default_can_be_changed_or_turned_off_and_a_bad_one_is_not_bound() {
        assert_eq!(parse("").keys.next_key(), Ok(Some(NEXT_KEY.to_string())));
        assert_eq!(parse("[keys]\nnext = \"M-n\"\n").keys.next_key(), Ok(Some("M-n".to_string())));
        assert_eq!(parse("[keys]\nnext = \"\"\n").keys.next_key(), Ok(None));
        assert!(parse("[keys]\nnext = \"n\"\n").keys.next_key().is_err());
        assert!(parse("[keys]\nnext = \"C-q\"\n").keys.next_key().is_err());
    }

    #[test]
    fn a_tool_that_names_no_builtin_kind_is_declared() {
        let c = parse(
            "[tools.aider]\nresume = [\"--session\", \"{conv}\"]\nbusy = ['Working\\.\\.\\.']\nenv = { A = \"1\" }\n\
             [tools.claude]\nenv = { B = \"2\" }\n",
        );
        let t = c.tool("aider").unwrap();
        assert_eq!((t.kind.as_str(), t.resume.len(), t.busy[0].as_str(), t.env["A"].as_str()), ("", 2, r"Working\.\.\.", "1"));
        // A built-in tool with other keys set still has itself as kind
        assert_eq!((c.tool("claude").unwrap().kind.as_str(), c.tool("claude").unwrap().env["B"].as_str()), ("claude", "2"));
        assert_eq!(c.names(), ["claude", "codex", "pi", "opencode", "shell", "aider"]);
    }

    #[test]
    fn an_account_is_a_directory_or_a_layer_in_the_harness_own_words() {
        let c = parse(
            "[accounts.codex]\ncorp = \"~/.corp-cx/\"\nlabs = { home = \"~/.labs\", args = [\"--profile\", \"labs\"] }\n\
             api = { env_file = \"~/k.env\", env = { OPENAI_BASE_URL = \"https://x\" } }\n",
        );
        let home = paths::expand("~");
        assert_eq!(c.account("codex", "corp").unwrap(), &Account { home: format!("{home}/.corp-cx"), ..Default::default() });
        let labs = c.account("codex", "labs").unwrap();
        assert_eq!(
            (labs.home.as_str(), labs.args.as_slice()),
            (format!("{home}/.labs").as_str(), ["--profile".to_string(), "labs".to_string()].as_slice())
        );
        // An account that keeps the directory: home is empty
        let api = c.account("codex", "api").unwrap();
        assert_eq!(
            (api.home.as_str(), api.env_file.as_str(), api.env["OPENAI_BASE_URL"].as_str()),
            ("", format!("{home}/k.env").as_str(), "https://x")
        );
        assert_eq!(c.names(), ["claude", "codex", "pi", "opencode", "shell", "codex:api", "codex:corp", "codex:labs"]);
    }

    #[test]
    fn login_lists_its_browsers_and_a_mistake_there_only_drops_that_section() {
        assert_eq!(parse("").login, Login::default());
        let c = parse(
            "[accounts.claude]\nsub = \"~/.cs\"\n[login]\nremote = \"no\"\n\
             browsers = [\"Firefox\", { name = \"Chrome 工作\", cmd = [\"google-chrome\", \"--profile-directory=Profile 1\"] }, { cmd = [\"~/bin/zen\", \"{url}\"] }]\n",
        );
        assert_eq!(c.login.remote, Remote::No);
        let b = &c.login.browsers;
        // A string: the App's name on Mac, a command elsewhere
        let firefox = if cfg!(target_os = "macos") { vec!["open", "-a", "Firefox"] } else { vec!["Firefox"] };
        assert_eq!((b[0].name.as_str(), b[0].cmd.iter().map(String::as_str).collect::<Vec<_>>()), ("Firefox", firefox));
        assert_eq!(b[1], Browser { name: "Chrome 工作".into(), cmd: vec!["google-chrome".into(), "--profile-directory=Profile 1".into()] });
        // No name: the command's file name; ~ expanded
        assert_eq!(b[2].name, "zen");
        assert!(b[2].cmd[0].ends_with("/bin/zen") && !b[2].cmd[0].starts_with('~'));
        // Shorthand as a path: the table also shows only the file name
        let b = parse("[login]\nbrowsers = [\"/opt/x/work-chrome\", \"/Applications/Arc.app\"]\n").login.browsers;
        assert_eq!((b[0].name.as_str(), b[1].name.as_str()), ("work-chrome", "Arc"));
        assert_eq!(parse("[login]\nremote = true\n").login.remote, Remote::Yes);
        assert_eq!(parse("[login]\nremote = \"auto\"\n").login.remote, Remote::Auto);
        // Written wrong: only [login] counts as unset, accounts are used as usual; it can say what's wrong
        let c = parse("[accounts.claude]\nsub = \"~/.cs\"\n[login]\nremote = \"maybe\"\nbrowsers = [\"Firefox\"]\n");
        assert!(c.account("claude", "sub").is_some() && c.login == Login::default());
        assert!(login_problem_in("[login]\nremote = \"maybe\"\n").is_some_and(|e| e.contains("auto / yes / no")));
        assert!(login_problem_in("[login]\nbrowsers = \"firefox\"\n").is_some());
        assert_eq!(login_problem_in("[login]\nremote = \"yes\"\n"), None);
        assert_eq!(login_problem_in("[ui]\nlang = \"zh\"\n"), None);
    }

    #[test]
    fn idle_stop_after_is_a_span_with_units_and_a_mistake_there_only_turns_it_off() {
        assert_eq!(parse("").idle.stop_after(), Ok(None));
        assert_eq!(parse("[idle]\nstop_after = \"3h\"\n").idle.stop_after(), Ok(Some(3 * 3600)));
        assert_eq!(parse("[idle]\nstop_after = \" 1H30m \"\n").idle.stop_after(), Ok(Some(5400)));
        assert_eq!(parse("[idle]\nstop_after = \"\"\n").idle.stop_after(), Ok(None));
        assert_eq!(parse("[idle]\nstop_after = \"0m\"\n").idle.stop_after(), Ok(None));
        assert_eq!((span("2d"), span("45s"), span("90m")), (Some(172800), Some(45), Some(5400)));
        // Not accepted: a last number with no unit, other words, a unit with no number before it
        assert_eq!((span("3"), span("3h20"), span("3 小时"), span("h"), span("99999999999999999999d")), (None, None, None, None, None));
        // Written wrong: no auto-stop, it can say what's wrong; the rest is used as usual
        let c = parse("[accounts.claude]\nsub = \"~/.cs\"\n[idle]\nstop_after = 180\n");
        assert!(c.account("claude", "sub").is_some() && c.idle.stop_after().is_err());
        assert!(idle_problem_in("[idle]\nstop_after = 180\n").is_some_and(|e| e.contains("\"3h\"")));
        assert!(idle_problem_in("[idle]\nstop_after = \"3 hours\"\n").is_some_and(|e| e.contains("3 hours")));
        assert!(idle_problem_in("idle = 3\n").is_some());
        assert_eq!(idle_problem_in("[idle]\nstop_after = \"3h\"\n"), None);
        assert_eq!(idle_problem_in("[keys]\nnext = \"\"\n"), None);
    }

    #[test]
    fn never_turns_either_one_off_and_remove_after_reads_like_stop_after() {
        let idle = |text: &str| parse(&format!("[idle]\n{text}\n")).idle;
        assert_eq!(idle("stop_after = \"never\"").stop_after(), Ok(None));
        assert_eq!(idle("stop_after = \" Never \"").stop_after(), Ok(None));
        assert_eq!(idle("remove_after = \"never\"").remove_after(), Ok(None));
        assert_eq!(idle("").remove_after(), Ok(None));
        assert_eq!(idle("remove_after = \"30d\"").remove_after(), Ok(Some(30 * 86400)));
        assert_eq!(idle_problem_in("[idle]\nstop_after = \"never\"\nremove_after = \"never\"\n"), None);
        // It can say which of the two is wrong; the other is used as usual
        let i = idle("stop_after = \"1d\"\nremove_after = \"一个月\"");
        assert_eq!(i.stop_after(), Ok(Some(86400)));
        assert!(i.remove_after().is_err());
        assert!(idle_problem_in("[idle]\nremove_after = 30\n").is_some_and(|e| e.contains("remove_after") && e.contains("\"30d\"")));
        assert!(idle_problem_in("[idle]\nremove_after = \"一个月\"\n").is_some_and(|e| e.contains("never")));
    }
}
