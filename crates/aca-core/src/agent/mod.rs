//! Each agent: how to start it, how hooks get passed in, how events become state, how to read its screen, where its title
//! comes from.
//!
//! Two layers, following agent-deck. Everything else (session operations, the hook entry point, the list) only knows the
//! [`Agent`] interface, never `if claude … else codex …`:
//! - **Built-in**: one file per tool, implementing [`Agent`], registered with one line in [`BUILTIN`]. For the tools with their
//!   own hook system, special arguments to put together, or a conversation record to read (Claude, Codex).
//! - **Declared**: a `[tools.<name>]` section in config.toml (the command, the arguments to resume a conversation, regexes that
//!   read the state off the screen…), no code needed; see [`declared`]. If the agent has its own plugin / extension mechanism,
//!   add a small forwarder that calls `aca hook <tool name>` per the hook protocol, and the state is as complete as a built-in's.
//!
//! The hook protocol (the same for built-in and declared tools): `aca hook <built-in tool's name | tool name>`, once per
//! event, with one JSON on stdin shaped like a Claude Code hook (event names and fields: see [`lifecycle`]). Codex hooks
//! already have this shape.
//!
//! Three shared rules:
//! - **Authentication is the harness's own business**. An account is just "the extra things passed when starting it"
//!   (config directory, environment variables, arguments; see [`Account`]), written in the tool's own terms, in one place;
//!   with no account aca adds nothing and the tool uses its defaults. Logging in goes through the tool's own command
//!   (`aca run <tool:account> -- login`, see [`crate::launch::direct`]). aca doesn't store or sync credentials, and writes nothing into
//!   the tool's config directory
//! - Hooks are **not installed into any config directory**; they're all passed at startup (Claude's `--settings`, Codex's
//!   `-c hooks.…`; ones that need a file use [`hook_file`], ones that need an environment variable use [`Agent::env`]).
//!   Several accounts don't each need an install, and your own hooks stay untouched; an agent started by hand inside a
//!   session doesn't carry these, so it can't report under the session's name.
//! - The command goes straight to tmux (not wrapped in `bash -c`): the pane's process is the agent itself, so signals reach
//!   it when stopping, and there are no quotes to escape.

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

/// Events to emit after handling a hook: (name, this event's own fields)
pub type Events = Vec<(&'static str, Value)>;

#[derive(Clone, Debug, PartialEq)]
pub enum How {
    /// A new conversation (for tools that let us choose the conversation id: the registered one)
    Fresh,
    /// Resume this conversation
    Resume(String),
    /// Branch a new conversation off this one
    Fork(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Launch {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    /// The opening message was passed as a startup argument (see [`Agent::opening`]); false = there's no such message, or this
    /// tool doesn't take it and it has to be typed in once it's up
    pub said: bool,
}

/// How messages from other sessions are delivered ([`crate::inbox`])
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Delivery {
    /// Ring the doorbell: it watches a file itself, and on each change runs a hook that takes the message and hands it to the
    /// model as a system prompt (Claude)
    Bell,
    /// Put it in its own queue; it starts a turn on its own once idle (Codex's `codex queue`)
    Queue,
    /// Paste into the input box and press Enter
    Type,
    /// Don't deliver: only leave it in the inbox (shell — typed in, it would become a command)
    Keep,
}

pub trait Agent: Sync {
    /// Built-in: the word written as `kind` in config.toml; declared: the tool name. `[accounts.<it>]` is looked up by this too
    fn name(&self) -> &str;

    /// Which of its environment variables gets the account's `home` (config directory); None = accounts can only set env / args
    fn home_var(&self) -> Option<&str> {
        None
    }

    /// The config directory it uses when the account gives none and the environment has none (`~/.claude`); None = it has no
    /// config directory
    fn default_home(&self) -> Option<&str> {
        None
    }

    /// What this conversation leaves in the config directory `home`, as paths relative to `home`, the conversation record first.
    /// Copied as-is to the new account's config directory when resuming under another account
    /// ([`crate::session::rehome`]). Not found / this tool can't resume = empty
    fn conv_files(&self, _home: &Path, _conv: &str) -> Vec<PathBuf> {
        vec![]
    }

    /// When resuming under another account, what the session's own `args` must become to match what it used before (Codex:
    /// each account's config.toml has a different default model). `log` is this conversation's record in the old account,
    /// `from` / `to` are the two accounts' config directories, `fixed` is the arguments fixed by the tool and the new account.
    /// Returns (the new arguments, one line for people; empty = nothing to say). By default nothing changes
    fn rehome_args(&self, args: &[String], _log: &Path, _from: &Path, _to: &Path, _fixed: &[String]) -> (Vec<String>, String) {
        (args.to_vec(), String::new())
    }

    /// Program name (used when the tool doesn't set its own `cmd`)
    fn program(&self) -> String {
        self.name().to_string()
    }

    /// The conversation id fixed when registering a new session (only for tools that take it as a startup argument); empty =
    /// wait for the hook to report it
    fn new_conv(&self) -> String {
        String::new()
    }

    /// Inside a sandbox, where no hook says which conversation it's on and the tool takes no id at start: the newest one it
    /// started in `dir` at or after `since` (seconds), found in its records under `home`, as (its id, the record). None = can't
    /// tell
    fn conv_started_in(&self, _home: &Path, _dir: &Path, _since: u64) -> Option<(String, PathBuf)> {
        None
    }

    fn can_fork(&self) -> bool {
        false
    }

    /// The command to run: the first item is the program name (not yet resolved to a full path). `aca` is the program the hooks
    /// call back into
    fn argv(&self, tool: &Tool, s: &Session, how: &How, aca: &str) -> Res<Vec<String>>;

    /// Extra environment at startup (for tools whose hooks / extensions must come in through environment variables, such as
    /// opencode's `OPENCODE_CONFIG_CONTENT`). `set` is what the tool and account layers already set (on a name clash what's
    /// returned here wins: merge it yourself if it needs merging)
    fn env(&self, _tool: &Tool, _aca: &str, _set: &[(String, String)]) -> Res<Vec<(String, String)>> {
        Ok(vec![])
    }

    /// Whether a restart can resume: `conv` is the current conversation's id
    fn resume(&self, _conv: &str, _r: &Run) -> How {
        How::Fresh
    }

    /// One hook event → how the state changes and which events to emit. A pure function. By default it follows the hook protocol
    fn on_hook(&self, r: &mut Run, payload: &Value, now: u64) -> Events {
        lifecycle::apply(r, payload, now)
    }

    /// Whether the hook should check the pane title every time (for titles that only exist there)
    fn title_in_pane(&self) -> bool {
        false
    }

    /// The title the agent gave itself: `r` is the state after this event, `pane_title` is the pane title (empty if not looked up).
    /// By default the one the hook reports (`title` in the protocol)
    fn title(&self, r: &Run, _pane_title: &str) -> Option<String> {
        (!r.hook_title.is_empty()).then(|| r.hook_title.clone())
    }

    /// The title that can be told without reading files. Each list refresh asks with the pane title
    /// ([`crate::status::follow_titles`]): for a session without hooks (adopted, not restarted yet) this is the only way to
    /// follow the title, and one with hooks needn't wait for the turn to end.
    /// By default the same as [`Agent::title`]; a tool whose `title` reads files writes one that doesn't
    fn live_title(&self, r: &Run, pane_title: &str) -> Option<String> {
        self.title(r, pane_title)
    }

    /// Read the screen (plain text, no colors). By default it can't: the state follows the hook
    fn screen(&self, _lines: &[String]) -> Seen {
        Seen::Unsure
    }

    /// Fingerprint of the tail of the text (the few lines above the input box): compared with the last look to tell "still
    /// streaming text" (see [`crate::status::look`]). The status bar below the input box isn't included: it refreshes every
    /// so often even when idle. None = can't tell (the default), don't look
    fn body(&self, _lines: &[String]) -> Option<u64> {
        None
    }

    /// How messages from other sessions are delivered. By default typed in
    fn delivery(&self) -> Delivery {
        Delivery::Type
    }

    /// For a [`Delivery::Queue`] tool: the arguments after its program that put a message in conversation `conv`'s queue.
    /// `shared` = hand it to its shared background service (the only way once that service is already running)
    fn queue_args(&self, _conv: &str, _text: &str, _shared: bool) -> Vec<String> {
        vec![]
    }

    /// The mark at the start of the input box (Claude's ❯, Codex's ›): before typing, check whether the input box has text you
    /// haven't sent. None = can't tell, don't check
    fn prompt_mark(&self) -> Option<char> {
        None
    }

    /// Keys to press when answering a permission prompt for it: (approve, deny). None = untested, don't press for it
    fn permission_keys(&self) -> Option<(&'static str, &'static str)> {
        None
    }

    /// Whether the opening message (`aca new --say`) can be passed as a startup argument: if so, the arguments to put before
    /// it, with the message last. The agent then carries it past the startup gates itself (trusting the folder, reviewing
    /// hooks) and sends it once up, without waiting for its input box to be drawn and typing into it.
    /// None = this tool doesn't take it / this message can't go that way: type it in once it's up (see
    /// [`crate::session::send_fresh`])
    fn opening(&self, _text: &str) -> Option<Vec<String>> {
        None
    }

    /// Whether the screen shows the "do you trust this folder" prompt it asks the first time it enters a directory: if so, the
    /// keys to press to choose "trust". Pressed for you after starting a session when `[trust] folder = true` (see
    /// [`crate::trust`]). None = not that prompt / this tool can't tell
    fn trust_keys(&self, _lines: &[String]) -> Option<Vec<&'static str>> {
        None
    }

    /// Keys to press to interrupt the running turn (`aca interrupt`), pressed one at a time with a pause in between (opencode
    /// needs Esc twice, and two sent in one go count as one). None = untested, don't press for it
    fn interrupt_keys(&self) -> Option<&'static [&'static str]> {
        None
    }

    /// Whether it reports a hook itself when interrupted (Codex reports `Interrupt`); for those that don't, aca marks it
    /// stopped itself after pressing and emits `turn.interrupted`
    fn reports_interrupt(&self) -> bool {
        false
    }

    /// The hook says it's running but the screen shows it has stopped: whether the turn has already ended without a hook
    /// (Codex never reports a turn that ends in an error, and doesn't report a normal finish either when the Stop hook
    /// isn't trusted). If so, an event to fill in for it, shaped per the hook protocol: `StopFailure` (with `error` and
    /// `error_kind`) or `Stop` (with `last_assistant_message`), plus `at` if it's known when it ended. See [`crate::hook::feed`]
    fn unreported_end(&self, _r: &Run) -> Option<serde_json::Value> {
        None
    }

    /// The raw text of the last reply in the conversation record. When the hook's copy (`Run::last`) is missing (an adopted
    /// session not yet restarted has no hooks) or truncated, a plugin's `input = "reply"` reads it here. Can't read by default
    fn last_reply(&self, _r: &Run) -> Option<String> {
        None
    }

    /// Whether this process name is it (for recognizing ones started by hand, when adopting). Default: the same name
    fn runs_as(&self, comm: &str) -> bool {
        comm == self.name()
    }

    /// Asked when adopting ([`crate::adopt`]) a running process aca didn't start: which conversation it's on and which
    /// arguments it carries. `home` is the config directory in its environment (None = its default), `argv` is its command line
    /// (empty if unreadable), `ours` says whether a directory is one aca's plugins bring to agents (those aren't carried: they're
    /// rebuilt on start). By default nothing is recognized: the conversation id waits for it to restart under aca and the hook to
    /// report it
    fn live(&self, _pid: u32, _home: Option<&str>, _argv: &[String], _ours: &dyn Fn(&str) -> bool) -> Live {
        Live::default()
    }

    /// Read from the conversation record when a turn ends: messages other sessions passed it through the agent's own channel
    /// since `since` (seconds), and whether this turn was opened by you. Nothing by default
    fn turn_record(&self, _r: &Run, _since: u64) -> Turn {
        Turn::default()
    }
}

/// What the conversation record says about the turn that just ended ([`Agent::turn_record`])
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Turn {
    /// Messages from other sessions that came in through the agent's own channel, not aca's inbox
    pub native: Vec<Native>,
    /// Another session's message or a background task's notice opened this turn, not you: what you last said yourself (empty
    /// if it's not in the part read), to keep as [`Run::prompt`]. None = you opened it, or it can't be told
    pub said: Option<String>,
}

/// One message another session passed straight to this one (Claude's SendMessage), as the receiver's record has it
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Native {
    /// The agent's own id for it (Claude: `msg_id`)
    pub id: String,
    pub ts: u64,
    /// The sending process as the agent checked it (Claude: `verifiedPeerPid`); 0 = not given
    pub pid: u32,
    /// The sender's address as it says it (Claude: its socket path, without `uds:`)
    pub addr: String,
    /// The sender's name as the agent knows it
    pub name: String,
    pub text: String,
}

/// A running process, recognized from what it leaves behind itself ([`Agent::live`])
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Live {
    /// The current conversation's id, and where its record is
    pub conv: String,
    pub transcript: String,
    /// The title it gave itself
    pub title: String,
    /// Arguments on its command line other than "which conversation to resume": passed on every start after adopting. None =
    /// not sure, don't carry them (see [`carry`])
    pub args: Option<Vec<String>>,
}

/// The arguments from a command line that can be carried over: drop the program name and what `skip` says (`skip` looks at
/// the run starting at this word and returns how many words to drop, 0 = keep). Only flags and single-word values are
/// carried. If there's a message in it (a word with whitespace, or two words in a row without `-`: most likely the message
/// it was started with), nothing is carried: not sure, and getting it wrong would resend that message on every restart
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

/// How many words a flag with a value takes at the start here: `--x value` is 2, `--x=value` is 1, not it = 0
pub(crate) fn valued(rest: &[String], names: &[&str]) -> usize {
    let Some(first) = rest.first() else { return 0 };
    if names.contains(&first.as_str()) {
        return 2;
    }
    usize::from(names.iter().any(|n| first.strip_prefix(n).is_some_and(|v| v.starts_with('='))))
}

/// A flag whose value is optional (`--resume [id]`): the next word only counts as its value if it doesn't start with `-`
pub(crate) fn optional(rest: &[String], names: &[&str]) -> usize {
    match valued(rest, names) {
        2 if rest.get(1).is_none_or(|v| v.starts_with('-')) => 1,
        n => n,
    }
}

/// The built-in tools. **Adding one = adding a file to this directory and a line here**
static BUILTIN: &[&dyn Agent] = &[&claude::Claude, &codex::Codex, &pi::Pi, &opencode::Opencode, &shell::Shell];

pub fn builtin(kind: &str) -> Option<&'static dyn Agent> {
    BUILTIN.iter().copied().find(|a| a.name() == kind)
}

pub fn kinds() -> impl Iterator<Item = &'static str> {
    BUILTIN.iter().map(|a| a.name())
}

/// The definition for a tool name; the built-in tools have one even when unset. A built-in name with no kind gets itself as kind;
/// other names with no kind are declared tools, and kind stays empty
pub fn tool(cfg: &Config, name: &str) -> Option<Tool> {
    let builtin = builtin(name).is_some();
    let mut t = cfg.tools.get(name).cloned().or_else(|| builtin.then(Tool::default))?;
    if t.kind.is_empty() && builtin {
        t.kind = name.to_string();
    }
    Some(t)
}

/// The names the `n` menu / `aca new` accept: tools, and "tool:account"
pub fn names(cfg: &Config) -> Vec<String> {
    let mut v: Vec<String> = kinds().map(String::from).collect();
    v.extend(cfg.tools.keys().filter(|k| builtin(k).is_none()).cloned());
    for (kind, accounts) in &cfg.accounts {
        v.extend(accounts.keys().map(|a| format!("{kind}:{a}")));
    }
    // A terminal inside each sandbox
    v.extend(cfg.sandbox.keys().map(|b| format!("shell:{b}")));
    v
}

/// The built-in tool whose process name matches (`claude` / `codex`); desktop apps and other programs = None
pub fn by_comm(comm: &str) -> Option<&'static str> {
    BUILTIN.iter().copied().find(|a| a.runs_as(comm)).map(|a| a.name())
}

/// The agent for a tool: for built-in ones that tool, for declared ones one made from the config
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

/// Removed from the environment before starting an agent: variables other sessions (agent-deck's, an outer Claude / Codex)
/// use to identify themselves. If the tmux server was started from inside one of those sessions, these get into every new
/// session, and your globally installed hooks would record this session's events under that one
pub(crate) const FOREIGN: &[&str] = &[
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
    // This one crosses accounts: Claude passes it to its child processes to say "get login credentials under this config
    // directory", ahead of CLAUDE_CONFIG_DIR. If kept, a session started with another account has that account's directory
    // but the outer account's login (tested on 2.1.288: even an empty directory showed as logged in)
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    // This one crosses accounts too: it names which Anthropic profile to use (Console / federated login in
    // `~/.config/anthropic`), ahead of the config directory's own login. If kept, starting another account from inside an
    // account that has it uses that profile's login (tested on 2.1.296: an empty directory reported the profile's org and email)
    "ANTHROPIC_PROFILE",
    "CODEX_THREAD_ID",
];

/// What [`crate::launch::direct`] removes on top: when run inside an aca session, these two would make its forwarder report to
/// that session
pub(crate) const OURS: &[&str] = &["ACA_SESSION_ID", "ACA_TOOL"];

/// Prefix an env that removes these variables (except ones the tool / account sets itself: you meant those). env execs the
/// command, same process
pub(crate) fn without(vars: &[&[&str]], set: &[(String, String)], mut cmd: Vec<String>) -> Vec<String> {
    let mut argv: Vec<String> = vec!["/usr/bin/env".into()];
    let gone = vars.iter().flat_map(|v| v.iter()).filter(|k| !set.iter().any(|(name, _)| name == *k));
    argv.extend(gone.flat_map(|k| ["-u".to_string(), k.to_string()]));
    argv.append(&mut cmd);
    argv
}

/// Hook commands (and the popup `aca ui --ref` opens) run through a shell: spaces and quotes in the program path are fine
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Find the command's full path in PATH: the tmux server's PATH isn't necessarily this terminal's
pub(crate) fn which(cmd: &str) -> Option<String> {
    if cmd.contains('/') {
        return Some(paths::expand(cmd));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(cmd)).find(|f| f.is_file()).map(|f| f.to_string_lossy().into_owned())
}

pub fn self_exe() -> String {
    self_path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "aca".into())
}

/// The path of this program. Written into hooks, the status bar and tmux settings, and ^L restarts by exec'ing it: after an
/// upgrade it must point at the newly installed one
pub fn self_path() -> Option<PathBuf> {
    std::env::current_exe().ok().map(|p| stable_exe(&p))
}

/// - Linux reads /proc/self/exe: once the program file is replaced (upgraded), " (deleted)" is appended. Strip it, and it's
///   the new version installed in place
/// - Installed with Homebrew: on Linux it reads the versioned `<prefix>/Cellar/aca/<version>/bin/aca` (macOS reads the
///   bin/aca link), and upgrading removes the old version. Use the unversioned `<prefix>/opt/aca/bin/aca` that brew keeps
///   pointing at the current one
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

/// Tool name → (agent, the tool's definition). The hook entry point gets a built-in tool's name (`aca hook claude`) or a
/// tool name, and goes through here too
pub fn resolve(cfg: &Config, name: &str) -> Res<(Handle, Tool)> {
    let tool = tool(cfg, name).ok_or_else(|| tr!("没有工具 {name}(可用:{})", "no tool {name} (there are: {})", names(cfg).join(" ")))?;
    let agent = match tool.kind.as_str() {
        "" => Handle::Declared(declared::Declared::new(name, &tool)?),
        kind => Handle::Builtin(builtin(kind).ok_or_else(|| {
            tr!(
                "工具 {name} 的 kind 为 {kind},内置的只有 {};省略 kind 即为声明式工具",
                "tool {name} has kind {kind}, but the built-ins are only {}; leave kind out for a declared tool",
                kinds().collect::<Vec<_>>().join(" / ")
            )
        })?),
    };
    Ok((agent, tool))
}

/// Which tool the session uses
pub fn of(cfg: &Config, s: &Session) -> Res<(Handle, Tool)> {
    resolve(cfg, &s.tool)
}

/// Whether the account is written as a directory (`~/.x`, `/abs`, `./rel`): no registration needed, used as the config
/// directory directly
pub fn is_dir(account: &str) -> bool {
    account.starts_with(['~', '/', '.'])
}

/// The account shown in the list: the name for registered ones, the directory name for ones written as a directory
pub fn account_label(account: &str) -> &str {
    if is_dir(account) { account.rsplit('/').find(|p| !p.is_empty()).unwrap_or(account) } else { account }
}

/// The session's account layer. No account = an empty layer: aca adds nothing and the harness uses its own defaults. Bound to a
/// sandbox: its `env` comes along (`sandbox_env`); on this Mac, so does the config directory in its workspace
pub fn account(cfg: &Config, agent: &dyn Agent, account: &str) -> Res<Account> {
    let mut layer = match account {
        "" => return Ok(Account::default()),
        dir if is_dir(dir) => Account { home: config::dir(dir), ..Default::default() },
        // A shell's account can be a sandbox's name: a terminal inside it (`aca new shell:work`)
        name if agent.name() == "shell" && cfg.account("shell", name).is_none() && cfg.sandbox.contains_key(name) => {
            Account { sandbox: name.into(), ..Default::default() }
        }
        name => cfg.account(agent.name(), name).cloned().ok_or_else(|| {
            tr!(
                "config.toml 的 [accounts.{}] 中没有 {name}(未登记的账号可直接写目录:{}:~/.xxx)",
                "no {name} under [accounts.{}] in config.toml (unregistered ones can be a directory: {}:~/.xxx)",
                agent.name(),
                agent.name()
            )
        })?,
    };
    if !layer.sandbox.is_empty() {
        let name = layer.sandbox.clone();
        let Some(b) = cfg.sandbox.get(&name) else {
            return Err(tr!(
                "账号 {account} 绑定的沙箱 {name} 不存在:config.toml 中没有 [sandbox.{name}]",
                "account {account} is bound to sandbox {name}, but there's no [sandbox.{name}] in config.toml"
            ));
        };
        if b.cage() && !matches!(agent.name(), "claude" | "codex" | "shell") {
            return Err(tr!(
                "沙箱目前只支持 Claude Code、Codex 和 shell,{} 的账号 {account} 不能绑定沙箱",
                "sandboxes only take Claude Code, Codex and shell for now; {}'s account {account} can't be bound to one",
                agent.name()
            ));
        }
        if !layer.home.is_empty() {
            return Err(tr!(
                "账号 {account} 绑定了沙箱 {name},不能再写 home:它的配置目录是沙箱 workspace 下的 {}/",
                "account {account} is bound to sandbox {name}, so it takes no home: its config directory is {}/ in the sandbox's workspace",
                agent.name()
            ));
        }
        layer.sandbox_env = b.env.clone();
        // On this Mac: started like any account, its config directory the one a cage's would be. None without a workspace,
        // or it would quietly use the tool's default directory (another account's login)
        if !b.cage() && agent.home_var().is_some() {
            if b.workspace.trim().is_empty() {
                return Err(tr!(
                    "[sandbox.{name}] 没写 workspace:账号 {account} 的配置目录要放在它下面({}/)",
                    "[sandbox.{name}] has no workspace: account {account}'s config directory goes in it ({}/)",
                    agent.name()
                ));
            }
            layer.home = b.home(agent.name()).to_string_lossy().into_owned();
        }
    }
    if !layer.home.is_empty() && agent.home_var().is_none() {
        return Err(tr!(
            "{} 未指定配置目录对应的环境变量(声明式工具写 home_var);未指定时账号中只能使用 env / args",
            "{} doesn't say which environment variable gets the config directory (declared tools set home_var); without it accounts can only use env / args",
            agent.name()
        ));
    }
    Ok(layer)
}

/// What "tool[:account]" resolves to: tool name, account (a name, or a directory's full path), which tool, the tool's
/// definition
pub struct Pick {
    pub tool: String,
    pub account: String,
    pub agent: Handle,
    pub def: Tool,
}

/// "claude:sub", "codex:~/.x", "codex". No account uses the tool's own default; one written as a directory becomes a full
/// path, and if it's exactly a registered account (the directory-only kind) its name is recorded: one account has one
/// name in the list
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

/// The `.env` format: one `KEY=value` per line, lines starting with `#` are comments, an optional leading `export `, quotes
/// around the value removed. The line number of a bad line goes in the Err (not its content: it holds a key)
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
        // Unquoted value: a space plus # starts a comment
        let v = quoted.unwrap_or_else(|| v.split(" #").next().unwrap_or(v).trim_end());
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

/// Environment variables of the tool and account layers (later ones override earlier ones): the tool's env, its sandbox's env,
/// the account's env_file, the account's env, the config directory
pub(crate) fn layered_env(agent: &dyn Agent, tool: &Tool, a: &Account) -> Res<Vec<(String, String)>> {
    let own = |m: &std::collections::BTreeMap<String, String>| m.iter().map(|(k, v)| (k.clone(), paths::expand(v))).collect::<Vec<_>>();
    let mut env = own(&tool.env);
    env.extend(own(&a.sandbox_env));
    if !a.env_file.is_empty() {
        let text = std::fs::read_to_string(&a.env_file)
            .map_err(|e| tr!("无法读取账号的 env_file {}:{e}", "can't read the account's env_file {}: {e}", a.env_file))?;
        env.extend(parse_env(&text).map_err(|n| tr!("{} 第 {n} 行不是 KEY=值 格式", "{} line {n} isn't KEY=value", a.env_file))?);
    }
    env.extend(own(&a.env));
    if let (Some(var), false) = (agent.home_var(), a.home.is_empty()) {
        env.push((var.to_string(), a.home.clone()));
    }
    Ok(env)
}

/// The config directory this tool and account actually use: what the account layer gives (a config directory, or that
/// variable set in env / env_file), else the one in the environment, else its default ([`Agent::default_home`]). None =
/// this tool has no config directory
pub fn home(cfg: &Config, tool: &str, account: &str) -> Res<Option<PathBuf>> {
    let (agent, def) = resolve(cfg, tool)?;
    let Some(var) = agent.home_var() else { return Ok(None) };
    let layer = self::account(cfg, &*agent, account)?;
    // Bound to a sandbox: the config directory it uses inside, mounted at the same path
    if let Some(b) = cfg.sandbox.get(&layer.sandbox) {
        return Ok(Some(b.home(agent.name())));
    }
    let env = layered_env(&*agent, &def, &layer)?;
    let set = env.into_iter().rev().find(|(k, _)| k == var).map(|(_, v)| v);
    let dir = set.or_else(|| std::env::var(var).ok().filter(|v| !v.is_empty())).or_else(|| agent.default_home().map(String::from));
    Ok(dir.map(|d| PathBuf::from(paths::expand(&d))))
}

/// This many bytes from the end of the file, starting at the next whole line (for reading the last few entries of a
/// conversation record)
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

/// Files hooks need (Claude's settings, extension scripts…) live in `state/hooks/<name>`: write it (untouched if the
/// content is the same) and return the path
pub(crate) fn hook_file(name: &str, content: &str) -> Res<PathBuf> {
    let path = paths::state_dir().join("hooks").join(name);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(content) {
        paths::write_atomic(&path, content).map_err(|e| tr!("无法写入 {}:{e}", "can't write {}: {e}", path.display()))?;
    }
    Ok(path)
}

/// Maximum bytes for the opening message: it ends up as one command-line argument, and the system allows 1 MB for a
/// command's arguments plus environment (macOS)
pub const OPENING_MAX: usize = 200_000;

/// [`Agent::opening`] shared by Claude and Codex: the message goes after `--`, so one starting with a dash isn't taken as a
/// flag. A single English word isn't passed: it would be taken as a subcommand (`claude doctor`, `codex resume`), so it's
/// typed in once it's up
pub(crate) fn opening_after_dashes(text: &str) -> Option<Vec<String>> {
    let word = text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    (!word).then(|| vec!["--".to_string()])
}

/// `aca exec-saying <file> <command…>`: exec that command, with the file's text appended as one last argument. Returning
/// means the exec failed. If the file is gone (the same command was rerun in the pane), start without the message
pub fn exec_saying(file: &str, argv: &[String]) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let text = std::fs::read_to_string(file).ok();
    let _ = std::fs::remove_file(file);
    let Some((program, rest)) = argv.split_first() else { return std::io::Error::other("no command") };
    std::process::Command::new(program).args(rest).args(text).exec()
}

/// Nobody is at this machine: if `[login] remote` says yes / no, follow it; otherwise decide ourselves: connected over SSH
/// (the same check `aca` uses when copying), or Linux with no graphical session. Then the login link must open on another device:
/// the kind that calls back to this machine can't finish (see [`crate::launch::quiet_login`])
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

/// Set when the aca interface starts a login: points at a file where the login link the tool wants to open is written (see
/// [`login_link`]); the interface sees it and pops up a browser picker
pub const LOGIN_LINK: &str = "ACA_LOGIN_LINK";

/// `aca <link>`: the tool uses aca as its browser when logging in (see [`crate::launch::quiet_login`]). If the aca interface wants the link
/// ([`LOGIN_LINK`] points at a file), write it there; otherwise open nothing — the tool already printed the link on screen.
/// Always exit 0: the tool takes it as opened, and goes on waiting for you to finish logging in in the browser
pub fn login_link(url: &str) -> i32 {
    hand_link(std::env::var_os(LOGIN_LINK).map(PathBuf::from), url);
    0
}

fn hand_link(to: Option<PathBuf>, url: &str) {
    let Some(f) = to else {
        return eprintln!("{}", tr!("aca 不会为登录打开浏览器,链接:{url}", "aca doesn't open a browser for logins; the link: {url}"));
    };
    // Write alongside first, then rename: the interface never reads a half-written one
    let part = f.with_extension("part");
    if let Err(e) = std::fs::write(&part, url).and_then(|()| std::fs::rename(&part, &f)) {
        eprintln!("{}", tr!("无法将登录链接发送到 aca 界面({}):{e}", "couldn't hand the login link to aca's UI ({}): {e}", f.display()));
    }
}

/// The "tool:account" layer's environment (the tool's env, env_file, the account's env, the config directory), without the
/// session's identity. This is what `aca run` passes; so does a shell the interface opens next to a session (the session's
/// tmux environment is empty, see `session::launch`)
pub fn account_env(cfg: &Config, spec: &str) -> Res<Vec<(String, String)>> {
    let p = pick(cfg, spec)?;
    let layer = account(cfg, &*p.agent, &p.account)?;
    layered_env(&*p.agent, &p.def, &layer)
}

/// Run one of the tool's own commands with this session's tool and account (Codex's `queue`): like [`crate::launch::direct`], only the
/// account layer's environment, only the given arguments, no session identity
pub fn tool_cmd(cfg: &Config, s: &Session, args: &[String]) -> Res<Launch> {
    let (agent, tool) = of(cfg, s)?;
    let layer = account(cfg, &*agent, &s.account)?;
    let mut cmd = base(&tool, &agent.program());
    cmd[0] = which(&cmd[0]).ok_or_else(|| tr!("PATH 中找不到 {}", "{} isn't on PATH", cmd[0]))?;
    cmd.extend(args.iter().cloned());
    let env = layered_env(&*agent, &tool, &layer)?;
    Ok(Launch { argv: without(&[FOREIGN, OURS], &env, cmd), env, said: false })
}

/// The common part of the command: the tool's own command if it set one, otherwise this tool's program name
pub(crate) fn base(tool: &Tool, program: &str) -> Vec<String> {
    if tool.cmd.is_empty() { vec![program.to_string()] } else { tool.cmd.clone() }
}

/// Arguments passed on every start: the tool's (the account's are already appended to them), then the session's own
pub(crate) fn extra(tool: &Tool, s: &Session) -> Vec<String> {
    tool.args.iter().chain(&s.args).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_and_opencode_get_the_same_servers_in_their_own_shapes() {
        let mcp = vec![("aca".to_string(), vec!["plugin".to_string(), "tools".to_string()])];
        let pi: serde_json::Value = serde_json::from_str(&pi::pi_mcp_config("/bin/aca", &mcp)).unwrap();
        assert_eq!(pi, serde_json::json!({ "aca": { "command": "/bin/aca", "args": ["plugin", "tools"], "exposure": "direct" } }));
        assert_eq!(
            serde_json::Value::Object(opencode::opencode_mcp("/bin/aca", &mcp)),
            serde_json::json!({ "aca": { "type": "local", "command": ["/bin/aca", "plugin", "tools"], "enabled": true } })
        );
    }

    #[test]
    fn the_flags_for_codex_and_the_file_for_claude_carry_the_session() {
        let args = vec!["plugin".to_string(), "mcp".into(), "aca.delegate".into()];
        let f = codex::codex_mcp_flag("aca-delegate", "/bin/aca", &args, &[("ACA_SESSION_ID".into(), "ab12".into())]);
        let (k, v) = f.split_once('=').unwrap();
        assert_eq!(k, "mcp_servers.aca-delegate");
        let t: toml::Table = toml::from_str(&format!("x = {v}")).unwrap();
        assert_eq!(t["x"]["command"].as_str(), Some("/bin/aca"));
        assert_eq!(t["x"]["args"].as_array().unwrap().len(), 3);
        assert_eq!(t["x"]["env"]["ACA_SESSION_ID"].as_str(), Some("ab12"));
        let c: serde_json::Value =
            serde_json::from_str(&claude::claude_mcp_config("/bin/aca", &[("aca".into(), vec!["plugin".into(), "tools".into()])])).unwrap();
        assert_eq!(c["mcpServers"]["aca"]["args"][1], "tools");
    }

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
        // An unregistered directory: record the full path, show the directory name in the list
        let (_, account, _) = picked(&cfg, "codex:/h/.newcodex/").unwrap();
        assert_eq!((account.as_str(), account_label(&account)), ("/h/.newcodex", ".newcodex"));
        assert_eq!(picked(&cfg, "codex:~/.x").unwrap().1, format!("{}/.x", paths::expand("~")));
        let here = std::env::current_dir().unwrap();
        assert_eq!(picked(&cfg, "codex:./cx").unwrap().1, here.join("cx").to_string_lossy());
        // Exactly a registered, directory-only account: record its name
        assert_eq!(picked(&cfg, "codex:/h/.corp-cx").unwrap().1, "corp");
        // Same directory, but that account carries more (arguments): not the same thing, don't borrow its name
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
        // Accounts that don't change the directory (only env / args) don't need home_var
        assert_eq!(picked(&cfg, "bare:key").unwrap().1, "key");
        // A misspelled kind doesn't quietly become a declared tool
        assert!(picked(&cfg, "typo").unwrap_err().contains("cluade"));
    }

    #[test]
    fn the_opening_message_rides_after_two_dashes_unless_it_could_be_a_subcommand() {
        let dashes = Some(vec!["--".to_string()]);
        assert_eq!(opening_after_dashes("登录页 500 了,看一下"), dashes);
        // Ones starting with a dash, or spanning lines, are passed too: what follows `--` isn't a flag
        assert_eq!(opening_after_dashes("-x 开头"), dashes);
        assert_eq!(opening_after_dashes("# Task\n\n- do it"), dashes);
        assert_eq!(opening_after_dashes("fix the login page"), dashes);
        // A single English word would be taken as a subcommand (claude doctor, codex resume): not passed, typed in once it's up
        for word in ["resume", "doctor", "mcp", "hi", "dry-run"] {
            assert_eq!(opening_after_dashes(word), None, "{word}");
        }
        assert_eq!(opening_after_dashes("继续"), dashes);
    }

    #[test]
    fn the_link_goes_to_the_file_the_ui_named() {
        let dir = std::env::temp_dir().join(format!("aca-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("login-claude-work.url");
        hand_link(Some(f.clone()), "https://claude.com/cai/oauth/authorize?x=1");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "https://claude.com/cai/oauth/authorize?x=1");
        // Written alongside, then renamed: no half-written file left
        assert!(!f.with_extension("part").exists());
        // A second login with a new link: overwrite
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
        // brew's opt/aca doesn't point here yet (mid-install): keep using the one in Cellar
        assert_eq!(stable_exe(&cellar), cellar);
        std::fs::create_dir_all(dir.join("opt")).unwrap();
        std::os::unix::fs::symlink("../Cellar/aca/0.1.0", dir.join("opt/aca")).unwrap();
        assert_eq!(stable_exe(&cellar), dir.join("opt/aca/bin/aca"));
        // Replaced by an upgrade on Linux: the same path now holds the new version
        let local = dir.join("bin/aca");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(&local, "").unwrap();
        assert_eq!(stable_exe(&dir.join("bin/aca (deleted)")), local);
        // Everything else unchanged: not under Cellar, or not deep enough under Cellar
        assert_eq!(stable_exe(&local), local);
        assert_eq!(stable_exe(Path::new("/x/Cellar/aca")), Path::new("/x/Cellar/aca"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
