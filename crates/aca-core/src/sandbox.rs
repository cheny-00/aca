//! Sandboxes, the special environments: accounts that don't run on this Mac as it is (`[sandbox.<name>]` in config.toml, and
//! `[accounts.claude] team = { sandbox = "work" }`). The interface lists them apart (`^E`) and shows them as `team@work`. Two
//! kinds:
//!
//! - **On this Mac** (no `ssh`): its sessions start like any other, with its `env` (a time zone, a proxy) under the account's
//!   own, and the account's config directory in its workspace (`<workspace>/codex`). Nothing is kept apart: only the
//!   programs that read those variables follow them. None of the rest below applies
//! - **A cage** (`ssh`): kept apart from this Mac. The cage itself isn't aca's: anything ssh gets into with directories of
//!   this Mac mounted at the same path (one is in its own project, a pair of Docker containers). Its `env` goes in with
//!   each command
//!
//! For a cage:
//!
//! - **Starting**: the account's sessions run the tool inside, over ssh, behind the relay as usual
//!   (`aca pty -- aca exec-sandbox <name> '<command>'`); see [`exec`]. When ssh can't connect, `up` runs first. Only the
//!   tool's own arguments go in: no hooks, aca's mod, plugin directories or MCP servers (they're files on this Mac), and of
//!   the environment only the account's own (`env`, `env_file`). The state comes from what the agent reports itself
//!   (OSC 7501, through ssh) and from the screen
//! - **Paths**: mounted at the same path on both sides, so nothing is translated. A session starts only in a mounted
//!   directory or the workspace's `worktrees/`. The workspace's `claude/` is the cage's Claude config directory: logins and
//!   conversation records are readable from this Mac at the same path
//! - **git**: the cage can write a repository's `.git/config` (`core.fsmonitor`, diff and filter programs) and hooks, which a
//!   git run on this Mac executes, as you. So every git aca runs itself on a mounted directory runs inside instead ([`git`]),
//!   whichever session it's for
//! - **Closed**: aca doesn't send work or messages in. No session started from another session (delegating), no `aca msg`
//!   from a session, no workers' notices to it; no carrying a conversation in or out with another account. Only aca keeps to
//!   this: an agent of yours on this Mac that can read the ssh key can still get in by itself
//! - **Taking work in** (`delegate = true`): other sessions may start its sessions and send them messages, and what those
//!   workers do comes back out to whoever delegated them — so whatever runs inside reaches an agent on this Mac. With no
//!   hooks inside, the turns come from what the agent reports itself ([`crate::hook::reported`]). The rest stays closed

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::agent::sh_quote;
use crate::config::{Config, Sandbox};
use crate::paths;
use crate::tr;

impl Sandbox {
    /// A cage reached over ssh; false = on this Mac, with its `env`
    pub fn cage(&self) -> bool {
        !self.ssh.trim().is_empty()
    }

    pub fn workspace(&self) -> PathBuf {
        PathBuf::from(paths::expand(self.workspace.trim()))
    }

    /// Where its sessions' worktrees go: `<workspace>/worktrees`
    pub fn worktrees(&self) -> PathBuf {
        self.workspace().join("worktrees")
    }

    /// The config directory the tool `kind` uses inside: `<workspace>/<kind>` (`claude/`)
    pub fn home(&self, kind: &str) -> PathBuf {
        self.workspace().join(kind)
    }

    /// The mounted directories as full paths (symlinks resolved when they exist, so they compare with a session's directory)
    fn mounted(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.dirs.iter().map(|d| real(&paths::expand(d.trim()))).collect();
        if !self.workspace.trim().is_empty() {
            v.push(real(&self.worktrees().to_string_lossy()));
        }
        v
    }

    /// Whether `dir` is inside one of its mounted directories (or its worktrees)
    pub fn holds(&self, dir: &Path) -> bool {
        let dir = real(&dir.to_string_lossy());
        self.mounted().iter().any(|m| !m.as_os_str().is_empty() && dir.starts_with(m))
    }
}

fn real(p: &str) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p))
}

/// The sandbox an account is bound to: (its name, its section). None = not bound. A binding to a section that isn't there
/// is refused earlier, by [`crate::agent::account`]
pub fn of<'a>(cfg: &'a Config, kind: &str, account: &str) -> Option<(&'a str, &'a Sandbox)> {
    let name = match cfg.account(kind, account) {
        Some(a) => a.sandbox.as_str(),
        // A shell takes a sandbox's name as its account: a terminal inside it (`aca new shell:work`)
        None if kind == "shell" => account,
        None => return None,
    };
    cfg.sandbox.get_key_value(name).filter(|_| !name.is_empty()).map(|(k, v)| (k.as_str(), v))
}

/// The sandbox a session's account is bound to
pub fn of_session<'a>(cfg: &'a Config, s: &crate::store::Session) -> Option<(&'a str, &'a Sandbox)> {
    let (agent, _) = crate::agent::of(cfg, s).ok()?;
    of(cfg, agent.name(), &s.account)
}

/// [`of`], only when it's a cage: what keeps it apart (closed, over ssh) applies to these only
pub fn cage_of<'a>(cfg: &'a Config, kind: &str, account: &str) -> Option<(&'a str, &'a Sandbox)> {
    of(cfg, kind, account).filter(|(_, b)| b.cage())
}

/// [`of_session`], only when it's a cage
pub fn cage_of_session<'a>(cfg: &'a Config, s: &crate::store::Session) -> Option<(&'a str, &'a Sandbox)> {
    of_session(cfg, s).filter(|(_, b)| b.cage())
}

/// The interface's workspace the sessions of tool `tool` (a tool name, `claude` or one of your own) on `account` go to by
/// default: their sandbox's `space`, when it's one character. None = not bound, or no space set
pub fn space(cfg: &Config, tool: &str, account: &str) -> Option<char> {
    let kind = crate::agent::tool(cfg, tool).map(|t| if t.kind.is_empty() { tool.to_string() } else { t.kind })?;
    let (_, b) = of(cfg, &kind, account)?;
    let mut c = b.space.trim().chars();
    c.next().filter(|_| c.next().is_none())
}

/// The cage `dir` is mounted into, if any
pub fn holding<'a>(cfg: &'a Config, dir: &Path) -> Option<(&'a str, &'a Sandbox)> {
    cfg.sandbox.iter().find(|(_, b)| b.cage() && b.holds(dir)).map(|(k, v)| (k.as_str(), v))
}

/// A command line for the remote shell: each word quoted; `cd` into `dir` first when given, then `exec` it with `env`
/// in front
pub fn remote(dir: Option<&str>, env: &[(String, String)], argv: &[String]) -> String {
    let mut out = String::new();
    if let Some(d) = dir {
        out.push_str(&format!("cd {} && ", sh_quote(d)));
    }
    out.push_str("exec ");
    if !env.is_empty() {
        out.push_str("env ");
        for (k, v) in env {
            out.push_str(&sh_quote(&format!("{k}={v}")));
            out.push(' ');
        }
    }
    out.push_str(&argv.iter().map(|a| sh_quote(a)).collect::<Vec<_>>().join(" "));
    out
}

/// [`remote`] for a terminal: the far side's own login shell (its `$SHELL`, which plain `ssh` starts too), `args` after it
pub fn remote_shell(dir: Option<&str>, env: &[(String, String)], args: &[String]) -> String {
    // `remote` with nothing to run ends right after `exec ` (or the env): the shell goes there, unquoted
    let mut out = remote(dir, env, &[]);
    out.push_str("\"$SHELL\" -l");
    for a in args {
        out.push(' ');
        out.push_str(&sh_quote(a));
    }
    out
}

/// ssh without asking anything: a key that needs a passphrase not in the agent, or an unknown host key, fails instead of
/// waiting for someone to type
fn quiet_ssh(host: &str) -> Command {
    let mut c = Command::new("ssh");
    c.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=5", host, "--"]);
    c
}

/// git on `dir` with these arguments (and environment variables): run here, or inside the sandbox `dir` is mounted into
/// (see the module docs). `-C dir` is added; stdin is closed
pub fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
    match holding(&crate::config::load(), dir) {
        None => {
            let mut c = Command::new("git");
            c.arg("-C").arg(dir).args(args).envs(env.iter().copied()).stdin(Stdio::null());
            c
        }
        Some((_, b)) => {
            let mut c = quiet_ssh(b.ssh.trim());
            let env: Vec<(String, String)> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            let argv: Vec<String> =
                ["git", "-C", &dir.to_string_lossy()].into_iter().chain(args.iter().copied()).map(String::from).collect();
            c.arg(remote(None, &env, &argv)).stdin(Stdio::null());
            c
        }
    }
}

/// Where ssh connects for this destination once ~/.ssh/config is applied: (host name, port), from `ssh -G` (connects nowhere)
fn endpoint(dest: &str) -> Option<(String, String)> {
    let o = Command::new("ssh").args(["-G", "--", dest]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&o.stdout);
    let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix(' ').map(|v| v.trim().to_string()));
    o.status.success().then(|| Some((field("hostname")?, field("port")?))).flatten()
}

/// The sandbox the ssh destination `dest` gets into, if any: its own `ssh` name, or any destination that connects to the same
/// address and port (another alias for it in ~/.ssh/config, `user@` in front). For tools that send work over ssh, such as
/// aca.remote: a sandbox takes none
pub fn reached_by<'a>(cfg: &'a Config, dest: &str) -> Option<&'a str> {
    let dest = dest.trim();
    let named = |b: &Sandbox| !b.ssh.trim().is_empty();
    if let Some((n, _)) = cfg.sandbox.iter().find(|(_, b)| named(b) && b.ssh.trim() == dest) {
        return Some(n);
    }
    if cfg.sandbox.is_empty() || dest.is_empty() {
        return None;
    }
    let at = endpoint(dest)?;
    cfg.sandbox.iter().find(|(_, b)| named(b) && endpoint(b.ssh.trim()).as_ref() == Some(&at)).map(|(n, _)| n.as_str())
}

fn reachable(host: &str) -> bool {
    quiet_ssh(host).arg("true").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Where Codex's browser login waits for the callback (`http://127.0.0.1:1455/auth/callback`, fixed by OpenAI): forwarded from
/// this Mac into the sandbox while logging in there (see `launch::direct`)
pub const CODEX_LOGIN_PORT: u16 = 1455;

/// `aca exec-sandbox [--forward <port>] <name> <command>`: exec ssh with that command, and with this Mac's 127.0.0.1:`port`
/// forwarded to the same inside when given (the cage's sshd has to allow it). Run from a terminal (a session's pane, `aca run`
/// typed by you): if ssh can't connect, the sandbox's `up` runs first, its output on screen, and the far side gets a
/// terminal too. Otherwise (output read by a program, such as the accounts page asking `claude auth status`): no terminal,
/// and nothing is started. Returning means it couldn't exec
pub fn exec(name: &str, command: &str, forward: Option<u16>) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let cfg = crate::config::load();
    let Some(b) = cfg.sandbox.get(name) else {
        return std::io::Error::other(tr!("config.toml 中没有 [sandbox.{name}]", "no [sandbox.{name}] in config.toml"));
    };
    let host = b.ssh.trim();
    if host.is_empty() {
        return std::io::Error::other(tr!("[sandbox.{name}] 未填写 ssh", "[sandbox.{name}] has no ssh"));
    }
    let tty = unsafe { libc::isatty(0) } == 1;
    let up = paths::expand(b.up.trim());
    if tty && !up.is_empty() && !reachable(host) {
        eprintln!("{}", tr!("无法连接 {host},正在运行 {up}", "can't reach {host}, running {up}"));
        match Command::new(&up).stdin(Stdio::null()).status() {
            Ok(s) if s.success() => {}
            Ok(s) => eprintln!("{}", tr!("{up} 退出码 {}", "{up} exited with {}", s.code().unwrap_or(-1))),
            Err(e) => eprintln!("{}", tr!("无法运行 {up}:{e}", "can't run {up}: {e}")),
        }
    }
    let mut ssh = Command::new("ssh");
    ssh.arg(if tty { "-tt" } else { "-T" });
    if let Some(port) = forward {
        // Taken here (most likely another Codex login still waiting): say so, rather than ssh's bind error
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
            return std::io::Error::other(tr!(
                "本机 {port} 端口已被占用(可能有别的 Codex 正在登录),请先结束它",
                "port {port} on this Mac is taken (most likely another Codex login waiting): end that first"
            ));
        }
        // Its own connection, not a shared one (ControlMaster in ~/.ssh/config): a forward handed to a shared connection
        // stays with it after this login ends, holding the port
        ssh.args(["-o", "ControlPath=none", "-o", "ExitOnForwardFailure=yes", "-L", &format!("127.0.0.1:{port}:127.0.0.1:{port}")]);
    }
    ssh.args([host, "--", command]).exec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_remote_command_quotes_every_word_and_execs() {
        let argv = ["claude", "--session-id", "c-1", "--", "it's here"].map(String::from);
        assert_eq!(
            remote(Some("/w/my app"), &[("K".into(), "a b".into())], &argv),
            "cd '/w/my app' && exec env 'K=a b' 'claude' '--session-id' 'c-1' '--' 'it'\\''s here'"
        );
        assert_eq!(remote(None, &[], &["git".to_string()]), "exec 'git'");
        // A terminal: the far side's own shell, left for it to expand
        assert_eq!(remote_shell(Some("/w"), &[], &[]), "cd '/w' && exec \"$SHELL\" -l");
        assert_eq!(remote_shell(None, &[("K".into(), "v".into())], &["-c".into(), "ls".into()]), "exec env 'K=v' \"$SHELL\" -l '-c' 'ls'");
    }

    #[test]
    fn a_sandbox_holds_its_mounted_dirs_and_worktrees_only() {
        let b = Sandbox { ssh: "work".into(), dirs: vec!["/w/app".into()], workspace: "/c/work".into(), ..Default::default() };
        assert!(b.holds(Path::new("/w/app")) && b.holds(Path::new("/w/app/src")));
        assert!(b.holds(Path::new("/c/work/worktrees/app/fix")));
        assert!(!b.holds(Path::new("/w/application")) && !b.holds(Path::new("/c/work/claude")) && !b.holds(Path::new("/w")));
        assert_eq!(b.home("claude"), PathBuf::from("/c/work/claude"));
    }

    #[test]
    fn a_sandboxed_accounts_sessions_default_to_its_space() {
        let cfg = crate::config::parse(
            "[sandbox.work]\nssh = \"work\"\nspace = \"3\"\n[sandbox.wide]\nssh = \"w\"\nspace = \"34\"\n\
             [tools.ccx]\nkind = \"claude\"\n[accounts.claude]\nteam = { sandbox = \"work\" }\nodd = { sandbox = \"wide\" }\nsub = \"~/.c\"\n",
        );
        assert_eq!((space(&cfg, "claude", "team"), space(&cfg, "ccx", "team")), (Some('3'), Some('3')));
        assert_eq!((space(&cfg, "claude", "sub"), space(&cfg, "claude", "odd"), space(&cfg, "codex", "team")), (None, None, None));
    }

    #[test]
    fn a_destination_reaching_the_same_address_and_port_is_the_sandbox() {
        let cfg = crate::config::parse("[sandbox.work]\nssh = \"dev@127.0.0.1\"\n[sandbox.none]\nssh = \"\"\n");
        assert_eq!(reached_by(&cfg, "dev@127.0.0.1"), Some("work"));
        // Another way to write the same place: ssh -G resolves both to 127.0.0.1 port 22
        assert_eq!(reached_by(&cfg, "other@127.0.0.1"), Some("work"));
        assert_eq!(reached_by(&cfg, "127.0.0.2"), None);
        assert_eq!(reached_by(&cfg, ""), None);
        assert_eq!(reached_by(&crate::config::Config::default(), "dev@127.0.0.1"), None);
    }

    #[test]
    fn git_on_an_unmounted_directory_runs_here() {
        let c = git(Path::new("/nowhere/at/all"), &["status"], &[]);
        assert_eq!(c.get_program(), "git");
    }
}
