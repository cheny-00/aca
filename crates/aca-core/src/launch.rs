//! How a session's agent is started: the account layer on top of the tool, what enabled plugins bring it, the opening
//! message, the relay, and an account bound to a cage started inside it over ssh ([`launch`]); `aca run`, one of the tool's
//! own commands with only the account's environment ([`direct`]), with logins kept off the default browser ([`quiet_login`]).
//!
//! Each agent only says how its own command line looks ([`Agent::argv`]); bringing in plugins and sandboxes happens here.

use std::path::Path;

use crate::agent::{
    Agent, FOREIGN, How, LOGIN_LINK, Launch, OURS, account, account_env, base, extra, layered_env, of, pick, remote, self_exe, which,
    without,
};
use crate::config::{Account, Config, Tool};
use crate::paths::{self, Res};
use crate::store::Session;
use crate::tr;

/// How to start this session this time: the account layer on top of the tool, the command resolved to a full path and
/// prefixed with an env that removes other sessions' identity, and the session id in the environment
///
/// `say` = the opening message: if this tool takes it as a startup argument ([`Agent::opening`]) it goes into a file, and
/// the command is prefixed with `aca exec-saying <file>`, which reads it and appends it last ([`crate::agent::exec_saying`]); otherwise
/// `Launch::said` is false
pub fn launch(cfg: &Config, s: &Session, how: &How, say: Option<&str>) -> Res<Launch> {
    let (agent, mut tool) = of(cfg, s)?;
    let layer = account(cfg, &*agent, &s.account)?;
    // A sandbox on this Mac starts like any account: the layer already carries its env and config directory
    if cfg.sandbox.get(&layer.sandbox).is_some_and(|b| b.cage()) {
        return inside(cfg, s, how, &*agent, tool, &layer);
    }
    // The account's arguments go after the tool's; each tool includes them when building its command ([`extra`])
    tool.args.extend(layer.args.iter().cloned());
    // The config's first, aca's own after: on a name clash the later one wins
    let mut env = layered_env(&*agent, &tool, &layer)?;
    // What enabled plugins give it (plugin directories, MCP servers; Codex also needs a copy it recognizes prepared)
    let codex_home = (agent.name() == "codex").then(|| codex_home(&env));
    let m = crate::plugin::mount::plan(agent.name(), &s.tool, &s.account, codex_home.as_deref());
    (tool.plugin_dirs, tool.mcp, tool.codex_plugins) = (m.dirs, m.mcp, m.codex_plugins);
    // Record what was passed this time: when plugins change, the Plugins page uses this to say which running sessions need
    // a restart
    let sig = crate::plugin::mount::signature(agent.name(), &s.tool, &s.account);
    if crate::runtime::load(&s.id).mounts != sig {
        crate::runtime::update(&s.id, |r| r.mounts = sig);
    }
    let aca = self_exe();
    let mut cmd = agent.argv(&tool, s, how, &aca)?;
    cmd[0] = which(&cmd[0]).ok_or_else(|| tr!("PATH 中找不到 {}", "{} isn't on PATH", cmd[0]))?;
    let lead = say.and_then(|text| Some((agent.opening(text)?, text)));
    let said = lead.is_some();
    if let Some((lead, text)) = lead {
        let file = opening_file(&s.id, text)?;
        cmd = [aca.clone(), "exec-saying".to_string(), file].into_iter().chain(cmd).chain(lead).collect();
    }
    let more = agent.env(&tool, &aca, &env)?;
    env.extend(more);
    env.extend([
        ("ACA_SESSION_ID".to_string(), s.id.clone()),
        ("ACA_TOOL".to_string(), s.tool.clone()),
        ("ACA_BIN".to_string(), aca.clone()),
    ]);
    // With another directory / another tmux server (tests), the session's hooks must write to the same place too
    for k in ["ACA_HOME", "ACA_TMUX_SOCKET"] {
        if let Some(v) = std::env::var(k).ok().filter(|v| !v.is_empty()) {
            env.push((k.to_string(), v));
        }
    }
    let argv = without(&[FOREIGN], &env, cmd);
    Ok(Launch { argv: relayed(agent.name(), &tool, &aca, argv), env, said })
}

/// [`launch`] for an account bound to a cage (see [`crate::sandbox`]): the tool's own command, run inside over ssh. The
/// opening message isn't passed as an argument (`said` is false): it's typed in once it's up
fn inside(cfg: &Config, s: &Session, how: &How, agent: &dyn Agent, mut tool: Tool, layer: &Account) -> Res<Launch> {
    let name = &layer.sandbox;
    let b = &cfg.sandbox[name];
    let shell = agent.name() == "shell";
    let held = b.holds(Path::new(&s.dir));
    // A terminal goes in from anywhere: outside the mounted directories it starts in the sandbox's home, as `ssh` does
    if !held && !shell {
        return Err(tr!(
            "{} 不在沙箱 {name} 挂载的目录中([sandbox.{name}] 的 dirs,或 workspace 下的 worktrees/)",
            "{} isn't in a directory mounted into sandbox {name} (its dirs, or worktrees/ in its workspace)",
            s.dir
        ));
    }
    tool.args.extend(layer.args.iter().cloned());
    // Found on its PATH by name: the tool's `cmd` and `env` are written for this Mac
    (tool.cmd, tool.bare) = (vec![], true);
    let aca = self_exe();
    let env = layered_env(agent, &Tool::default(), layer)?;
    let dir = held.then_some(s.dir.as_str());
    // A terminal: the sandbox's own shell, not this Mac's $SHELL
    let line = if shell {
        crate::sandbox::remote_shell(dir, &env, &extra(&tool, s))
    } else {
        crate::sandbox::remote(dir, &env, &agent.argv(&tool, s, how, &aca)?)
    };
    let argv = vec![aca.clone(), "exec-sandbox".into(), name.clone(), line];
    let mut here =
        vec![("ACA_SESSION_ID".to_string(), s.id.clone()), ("ACA_TOOL".to_string(), s.tool.clone()), ("ACA_BIN".to_string(), aca.clone())];
    for k in ["ACA_HOME", "ACA_TMUX_SOCKET"] {
        if let Some(v) = std::env::var(k).ok().filter(|v| !v.is_empty()) {
            here.push((k.to_string(), v));
        }
    }
    let argv = without(&[FOREIGN], &here, argv);
    Ok(Launch { argv: relayed(agent.name(), &tool, &aca, argv), env: here, said: false })
}

/// The agents known to report their own state (OSC 7501): Claude Code (2.1.322 on, after the terminal answers its query) and
/// Codex (0.167 on, without asking). Only these get the relay by default; for the others it would be one more layer for nothing
const REPORTING: &[&str] = &["claude", "codex"];

/// The agent behind aca's relay (`aca pty -- …`, see [`crate::relay`]), so the state it reports itself reaches aca. By default
/// only the agents known to report ([`REPORTING`]); `program_status = true` puts any other one behind it too (one that learned
/// it later), `false` none. Never shell: the list follows a shell's working directory through tmux, which would then only see
/// the relay's
fn relayed(name: &str, tool: &Tool, aca: &str, argv: Vec<String>) -> Vec<String> {
    let on = name != "shell" && tool.program_status.unwrap_or(REPORTING.contains(&name));
    if !on {
        return argv;
    }
    [aca.to_string(), "pty".into(), "--".into()].into_iter().chain(argv).collect()
}

/// Which config directory this Codex uses: the account's `CODEX_HOME`, else the one in the environment, else `~/.codex`
fn codex_home(env: &[(String, String)]) -> std::path::PathBuf {
    let from_env = env.iter().rev().find(|(k, _)| k == "CODEX_HOME").map(|(_, v)| v.clone());
    let home = from_env.or_else(|| std::env::var("CODEX_HOME").ok().filter(|v| !v.is_empty()));
    home.map(std::path::PathBuf::from).unwrap_or_else(|| paths::home().join(".codex"))
}

/// The opening message is kept in `state/opening/<session id>`, readable only by us, and [`crate::agent::exec_saying`] deletes it after
/// reading. Not written into the command directly: the command goes through tmux, which caps a command at a dozen-odd KB,
/// too small for a brief
fn opening_file(id: &str, text: &str) -> Res<String> {
    use std::os::unix::fs::PermissionsExt;
    let path = paths::state_dir().join("opening").join(id);
    paths::write_atomic(&path, text).map_err(|e| tr!("无法写入 {}:{e}", "can't write {}: {e}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    Ok(path.to_string_lossy().into_owned())
}

/// `aca run`: no session, just run the tool's own command as-is (`login`, `mcp`, `doctor`…), with only the account layer's
/// **environment** — the same as typing `CODEX_HOME=~/.codex2 codex login` in a terminal, minus typing the directory again.
/// The arguments are only yours: the tool's and account's `args` are for starting sessions, and management commands may
/// not accept them (Codex's `login` errors out with `--profile`), so add them yourself after. No hooks either
pub fn direct(cfg: &Config, spec: &str, args: &[String]) -> Res<Launch> {
    let p = pick(cfg, spec)?;
    // Bound to a cage: run it inside (logging in there too), with the account's own environment
    if let Some((name, _)) = crate::sandbox::cage_of(cfg, p.agent.name(), &p.account) {
        let layer = account(cfg, &*p.agent, &p.account)?;
        let env = layered_env(&*p.agent, &Tool::default(), &layer)?;
        let mut cmd: Vec<String> = std::iter::once(p.agent.program()).chain(args.iter().cloned()).collect();
        // Nothing inside opens a browser. Codex's login waits in there for the browser's callback on 127.0.0.1:1455: this Mac's
        // port is forwarded to it for the login, so the browser here finishes it (a ChatGPT Business workspace may not allow
        // device codes). Nobody at this machine (see [`remote`]): that browser is elsewhere, so a device code. Claude, finding
        // no browser there, shows the link whose code you paste back by itself
        let mut argv = vec![self_exe(), "exec-sandbox".into()];
        if p.agent.name() == "codex" && browser_login("codex", args) {
            match remote(cfg) {
                true => cmd.push("--device-auth".into()),
                false => argv.extend(["--forward".into(), crate::sandbox::CODEX_LOGIN_PORT.to_string()]),
            }
        }
        argv.extend([name.to_string(), crate::sandbox::remote(None, &env, &cmd)]);
        return Ok(Launch { argv: without(&[FOREIGN, OURS], &[], argv), env: vec![], said: false });
    }
    let mut cmd = base(&p.def, &p.agent.program());
    cmd[0] = which(&cmd[0]).ok_or_else(|| tr!("PATH 中找不到 {}", "{} isn't on PATH", cmd[0]))?;
    cmd.extend(args.iter().cloned());
    let mut env = account_env(cfg, spec)?;
    let argv = without(&[FOREIGN, OURS], &env, cmd);
    let asked = std::env::var_os(LOGIN_LINK).is_some();
    let theirs = std::env::var_os("BROWSER").is_some_and(|b| !b.is_empty());
    let argv = quiet_login(p.agent.name(), args, Seat { asked, theirs, remote: remote(cfg) }, &mut env, argv);
    Ok(Launch { argv, env, said: false })
}

/// Circumstances of a login: asked = started by the aca interface (with [`LOGIN_LINK`]); theirs = BROWSER set outside;
/// remote see [`remote`]
#[derive(Clone, Copy, Default)]
struct Seat {
    asked: bool,
    theirs: bool,
    remote: bool,
}

/// On macOS Codex opens links only through the system's open and ignores `$BROWSER`: run it in a sandbox that blocks only
/// "open another program" (callbacks to this machine still arrive)
const SANDBOX: &str = "/usr/bin/sandbox-exec";
const NO_OPEN: &str = "(version 1) (allow default) (deny lsopen)";

/// The tool's own browser login: `claude auth login`, `codex login` (not with a key / token, device code, status check or help)
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

/// Logging in doesn't open your default browser for you (the user, 2026-10-14: this account isn't necessarily logged in from
/// the default browser, and switching over by hand is a pain). Both tools print the link on screen themselves; Claude opens
/// links with `$BROWSER`, which points back at aca (see [`crate::agent::login_link`]); Codex ignores it, so it's sandboxed to block that.
/// Started by the aca interface (asked): done whatever is run (a new account starts the tool itself), overriding a BROWSER
/// set outside, since the link must go to the interface; typed by you: only login commands, and a BROWSER set outside
/// (theirs) is yours to use. A BROWSER set in the account always wins.
/// Remote (the user's pick, 2026-10-14): switch to a login that can finish on another device — Claude gets BROWSER=`true`,
/// so it opens nothing and shows the link whose code you paste back; Codex gets `--device-auth` (a fixed link + a one-time
/// code), with no callback to this machine
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::home;
    use crate::config;
    use std::path::PathBuf;

    #[test]
    fn only_agents_known_to_report_their_state_go_behind_the_relay() {
        let argv = || vec!["/usr/bin/env".to_string(), "/bin/x".to_string()];
        let behind =
            |name: &str, on: Option<bool>| relayed(name, &Tool { program_status: on, ..Default::default() }, "/aca", argv())[0] == "/aca";
        assert!(behind("claude", None) && behind("codex", None));
        assert_eq!(relayed("claude", &Tool::default(), "/aca", argv())[..3], ["/aca", "pty", "--"]);
        assert!(!behind("pi", None) && !behind("opencode", None) && !behind("aider", None));
        // Turned on for one that learned it later, off for one that does report
        assert!(behind("pi", Some(true)) && !behind("claude", Some(false)));
        // Never shell
        assert!(!behind("shell", Some(true)));
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
        // Arguments: the tool's, the account's, the session's own
        assert_eq!(l.argv[l.argv.len() - 6..], ["/bin/sh", "toy.sh", "--yolo", "--profile", "w", "--mine"]);
        // Environment: on a name clash later overrides earlier, the last one counts
        let last = |k: &str| l.env.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(
            (last("TOY_KEEP"), last("TOY_KEY"), last("TOY_URL"), last("TOY_MODE"), last("TOY_HOME")),
            (Some("1"), Some("from file"), Some("https://x"), Some("inline"), Some("/h/.toy-work"))
        );
        assert_eq!((last("ACA_SESSION_ID"), last("ACA_TOOL")), (Some("ab12"), Some("toy")));

        // No account: aca adds nothing
        let bare = launch(&cfg, &Session { account: String::new(), ..s.clone() }, &How::Fresh, None).unwrap();
        assert!(bare.env.iter().all(|(k, _)| !matches!(k.as_str(), "TOY_HOME" | "TOY_KEY" | "TOY_MODE")));
        // Unregistered, written as a directory: just the config directory
        let adhoc = launch(&cfg, &Session { account: "/h/.toy-new".into(), ..s.clone() }, &How::Fresh, None).unwrap();
        assert_eq!(adhoc.env.iter().find(|(k, _)| k == "TOY_HOME").map(|(_, v)| v.as_str()), Some("/h/.toy-new"));
        assert_eq!(adhoc.argv[adhoc.argv.len() - 4..], ["/bin/sh", "toy.sh", "--yolo", "--mine"]);

        // Opening message: a declared tool doesn't take it as a startup argument, so the command is unchanged and whoever starts
        // it waits for it to come up and types it in
        let told = launch(&cfg, &s, &How::Fresh, Some("先看一下 README")).unwrap();
        assert_eq!((told.said, &told.argv), (false, &l.argv));

        // `aca run`: the tool's program + your arguments, the account only adds environment; no session-start arguments, no
        // session identity
        let d = direct(&cfg, "toy:work", &["login".to_string()]).unwrap();
        assert_eq!(d.argv[d.argv.len() - 4..], ["ACA_TOOL", "/bin/sh", "toy.sh", "login"]);
        assert!(d.argv.windows(2).any(|w| w == ["-u", "ACA_SESSION_ID"]) && d.argv.windows(2).any(|w| w == ["-u", "CLAUDECODE"]));
        assert!(d.env.iter().any(|(k, v)| k == "TOY_HOME" && v == "/h/.toy-work") && d.env.iter().all(|(k, _)| !k.starts_with("ACA_")));

        // env_file missing / broken: say where, without leaking the content
        std::fs::write(&file, "TOY_KEY=k\n这行不对 sk-secret\n").unwrap();
        let err = launch(&cfg, &s, &How::Fresh, None).unwrap_err();
        assert!(err.contains("第 2 行") && !err.contains("sk-secret"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(launch(&cfg, &s, &How::Fresh, None).unwrap_err().contains("env_file"));
    }

    #[test]
    fn an_account_bound_to_a_sandbox_starts_inside_over_ssh() {
        let root = std::env::temp_dir().join(format!("aca-sandbox-{}", std::process::id()));
        std::fs::create_dir_all(root.join("app")).unwrap();
        let app = root.join("app").canonicalize().unwrap();
        let ws = root.join("ws");
        let cfg_text = format!(
            "[tools.claude]\nargs = [\"--model\", \"opus\"]\nenv = {{ MAC_ONLY = \"1\" }}\ncmd = [\"/mac/wrapper\"]\n\
             [sandbox.work]\nssh = \"work\"\ndirs = [\"{}\"]\nworkspace = \"{}\"\n\
             [accounts.claude]\nteam = {{ sandbox = \"work\", env = {{ TZ = \"Asia/Shanghai\" }} }}\n\
             lost = {{ sandbox = \"nope\" }}\nboth = {{ sandbox = \"work\", home = \"/h/.c\" }}\n\
             [accounts.codex]\ncx = {{ sandbox = \"work\" }}\n[accounts.opencode]\noc = {{ sandbox = \"work\" }}\n",
            app.display(),
            ws.display()
        );
        let cfg = config::parse(&cfg_text);
        let s = Session {
            id: "ab12".into(),
            tool: "claude".into(),
            account: "team".into(),
            dir: app.to_string_lossy().into_owned(),
            conv: "c-1".into(),
            ..Default::default()
        };
        // Behind the relay, the step that reaches the sandbox, and the command for its shell: the tool's own program and
        // arguments, the account's environment; nothing of this Mac (the tool's cmd and env, hooks, aca's files)
        let l = launch(&cfg, &s, &How::Fresh, Some("先看一下 README")).unwrap();
        let n = l.argv.len();
        assert_eq!(l.argv[1..3], ["pty", "--"]);
        assert_eq!(l.argv[n - 3..n - 1], ["exec-sandbox", "work"]);
        assert_eq!(
            l.argv[n - 1],
            format!("cd '{}' && exec env 'TZ=Asia/Shanghai' 'claude' '--session-id' 'c-1' '--model' 'opus'", app.display())
        );
        // The opening message is typed in once it's up; only aca's own variables stay here
        assert!(!l.said && l.env.iter().all(|(k, _)| k.starts_with("ACA_")));
        assert!(l.argv.windows(2).any(|w| w == ["-u", "CLAUDECODE"]));
        // Only in the directories mounted into it
        let out = launch(&cfg, &Session { dir: root.to_string_lossy().into_owned(), ..s.clone() }, &How::Fresh, None).unwrap_err();
        assert!(out.contains("sandbox work") || out.contains("沙箱 work"), "{out}");
        // Bound wrong: said where
        let err = |account: &str, tool: &str| {
            launch(&cfg, &Session { account: account.into(), tool: tool.into(), ..s.clone() }, &How::Fresh, None).unwrap_err()
        };
        assert!(err("lost", "claude").contains("[sandbox.nope]"));
        assert!(err("both", "claude").contains("home"));
        assert!(err("oc", "opencode").contains("Codex"));
        // Codex too, with only its own arguments: no hooks, session variables or plugins (files and commands of this Mac)
        let cx = Session { tool: "codex".into(), account: "cx".into(), conv: String::new(), ..s.clone() };
        let l = launch(&cfg, &cx, &How::Fresh, None).unwrap();
        assert_eq!(l.argv[l.argv.len() - 1], format!("cd '{}' && exec 'codex'", app.display()));
        let l = launch(&cfg, &cx, &How::Resume("th-1".into()), None).unwrap();
        assert!(l.argv[l.argv.len() - 1].ends_with("exec 'codex' 'resume' 'th-1'"));
        // Logging in inside: the browser here finishes it, its callback forwarded in; nobody at this machine, a device code
        let login =
            |cfg: &Config, args: &[&str]| direct(cfg, "codex:cx", &args.iter().map(|a| a.to_string()).collect::<Vec<_>>()).unwrap().argv;
        let here = config::parse(&format!("{cfg_text}[login]\nremote = \"no\"\n"));
        let l = login(&here, &["login"]);
        assert_eq!(l[l.len() - 5..l.len() - 1], ["exec-sandbox", "--forward", "1455", "work"]);
        assert!(l.last().unwrap().ends_with("exec 'codex' 'login'"));
        let away = config::parse(&format!("{cfg_text}[login]\nremote = \"yes\"\n"));
        let l = login(&away, &["login"]);
        assert_eq!(l[l.len() - 3..l.len() - 1], ["exec-sandbox", "work"]);
        assert!(l.last().unwrap().ends_with("exec 'codex' 'login' '--device-auth'"));
        let l = login(&here, &["login", "status"]);
        assert_eq!(l[l.len() - 3..l.len() - 1], ["exec-sandbox", "work"]);
        assert!(l.last().unwrap().ends_with("exec 'codex' 'login' 'status'"));
        assert_eq!(home(&cfg, "codex", "cx").unwrap(), Some(ws.join("codex")));
        // A terminal inside, named by the sandbox: its own login shell; from outside the mounted directories, in its home
        let sh = Session { tool: "shell".into(), account: "work".into(), conv: String::new(), ..s.clone() };
        let l = launch(&cfg, &sh, &How::Fresh, None).unwrap();
        assert_eq!(l.argv[l.argv.len() - 3..], ["exec-sandbox", "work", &format!("cd '{}' && exec \"$SHELL\" -l", app.display())]);
        let far = launch(&cfg, &Session { dir: root.to_string_lossy().into_owned(), ..sh.clone() }, &How::Fresh, None).unwrap();
        assert_eq!(far.argv.last().unwrap(), "exec \"$SHELL\" -l");
        assert_eq!(crate::sandbox::of(&cfg, "shell", "work").map(|(n, _)| n), Some("work"));
        assert!(pick(&cfg, "shell:nope").is_err() && crate::sandbox::of(&cfg, "shell", "nope").is_none());
        // `aca run` runs it inside too (logging in there); its config directory is the one it uses inside
        let d = direct(&cfg, "claude:team", &["auth".to_string(), "status".to_string()]).unwrap();
        assert_eq!(d.argv[d.argv.len() - 3..], ["exec-sandbox", "work", "exec env 'TZ=Asia/Shanghai' 'claude' 'auth' 'status'"]);
        assert!(d.env.is_empty());
        assert_eq!(home(&cfg, "claude", "team").unwrap(), Some(ws.join("claude")));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_sandbox_on_this_mac_starts_like_any_account_with_its_env_and_config_dir() {
        let cfg = config::parse(
            "[tools.codex]\ncmd = [\"/bin/sh\"]\n\
             [sandbox.unis]\nworkspace = \"/w/unis\"\nenv = { TZ = \"America/Los_Angeles\", HTTPS_PROXY = \"http://127.0.0.1:28888\" }\n\
             [sandbox.bare]\nenv = { TZ = \"UTC\" }\n\
             [accounts.codex]\ntu = { sandbox = \"unis\", env = { TZ = \"Asia/Tokyo\" } }\nnows = { sandbox = \"bare\" }\n\
             [accounts.opencode]\noc = { sandbox = \"unis\" }\n",
        );
        // `aca run` runs it here, no ssh: its env under the account's own, its config directory in the workspace
        let d = direct(&cfg, "codex:tu", &["login".to_string(), "status".to_string()]).unwrap();
        assert!(!d.argv.iter().any(|a| a == "exec-sandbox"), "{:?}", d.argv);
        let get = |k: &str| d.env.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(
            (get("TZ"), get("HTTPS_PROXY"), get("CODEX_HOME")),
            (Some("Asia/Tokyo"), Some("http://127.0.0.1:28888"), Some("/w/unis/codex"))
        );
        assert_eq!(home(&cfg, "codex", "tu").unwrap(), Some(PathBuf::from("/w/unis/codex")));
        // Not a cage: none of what keeps one apart applies
        assert_eq!(crate::sandbox::of(&cfg, "codex", "tu").map(|(n, _)| n), Some("unis"));
        assert!(crate::sandbox::cage_of(&cfg, "codex", "tu").is_none());
        // A terminal in it; any tool can be bound; without a workspace there's nowhere for its login
        assert!(pick(&cfg, "shell:unis").is_ok() && pick(&cfg, "opencode:oc").is_ok());
        assert!(pick(&cfg, "codex:nows").err().is_some_and(|e| e.contains("workspace")));
    }

    #[test]
    fn the_outer_sessions_login_is_stripped_unless_the_account_asks_for_it() {
        let cfg = config::parse(
            "[tools.claude]\ncmd = [\"/bin/sh\"]\n[accounts.claude]\nfirm = \"/h/.cd\"\n\
             shared = { home = \"/h/.cd\", env = { CLAUDE_SECURESTORAGE_CONFIG_DIR = \"/h/.c\" } }\n\
             console = { env = { ANTHROPIC_PROFILE = \"work\" } }\n",
        );
        let unset = |spec: &str, var: &str| direct(&cfg, spec, &[]).unwrap().argv.windows(2).any(|w| w == ["-u", var]);
        // "Where to get credentials" left in the environment by an outer Claude: if not removed, firm's session uses the outer
        // account's login
        assert!(unset("claude:firm", "CLAUDE_SECURESTORAGE_CONFIG_DIR") && unset("claude", "CLAUDE_SECURESTORAGE_CONFIG_DIR"));
        // Set in the account itself: you meant it, so keep it
        assert!(!unset("claude:shared", "CLAUDE_SECURESTORAGE_CONFIG_DIR") && unset("claude:shared", "CLAUDECODE"));
        // Same for a profile named outside: if not removed, firm uses that profile's login; one the account names itself is kept
        assert!(unset("claude:firm", "ANTHROPIC_PROFILE") && unset("claude", "ANTHROPIC_PROFILE"));
        assert!(!unset("claude:console", "ANTHROPIC_PROFILE") && unset("claude:console", "CLAUDE_SECURESTORAGE_CONFIG_DIR"));
    }

    #[test]
    fn logins_aca_runs_dont_open_the_default_browser() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // Recognizes both tools' browser logins; not with a key, device code, status check or help
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
        // A login typed by you: BROWSER points back at aca, the command is unchanged
        assert_eq!(run(&login, here, &[]), (argv.clone(), Some(self_exe())));
        // A BROWSER set outside is yours to use; one set in the account wins, also when started by the interface
        assert_eq!(run(&login, Seat { theirs: true, ..here }, &[]).1, None);
        assert_eq!(run(&login, Seat { asked: true, ..here }, &[("BROWSER", "/x/ff")]).1.as_deref(), Some("/x/ff"));
        // Started by the interface: done even when it isn't a login command (a new account starts claude itself), overriding an
        // outside BROWSER
        assert_eq!(run(&[], Seat { asked: true, theirs: true, ..here }, &[]).1, Some(self_exe()));
        // Other commands: unchanged
        assert_eq!(run(&s(&["-p", "hi"]), here, &[]), (argv.clone(), None));
        // Remote: Claude opens nothing (BROWSER=true) and shows the link to paste a code from; one set outside is still yours
        let far = Seat { remote: true, ..here };
        assert_eq!(run(&login, far, &[]), (argv.clone(), Some("true".into())));
        assert_eq!(run(&[], Seat { asked: true, ..far }, &[]).1.as_deref(), Some("true"));
        assert_eq!(run(&login, Seat { theirs: true, ..far }, &[]).1, None);

        // Codex ignores BROWSER: on macOS it's sandboxed, blocking only opening other programs
        let cx = s(&["/usr/bin/env", "/bin/codex", "login"]);
        let sandboxed = cfg!(target_os = "macos") && std::path::Path::new(SANDBOX).exists();
        let strip = |out: Vec<String>| if sandboxed { out[3..].to_vec() } else { out };
        let out = quiet_login("codex", &s(&["login"]), here, &mut vec![], cx.clone());
        if sandboxed {
            assert_eq!(out[..3], s(&[SANDBOX, "-p", NO_OPEN])[..]);
        }
        assert_eq!(strip(out), cx);
        assert_eq!(quiet_login("codex", &s(&["login", "status"]), here, &mut vec![], cx.clone()), cx);
        // Remote: use the device code, no callback to this machine; nothing added when it isn't a login
        assert_eq!(
            strip(quiet_login("codex", &s(&["login"]), far, &mut vec![], cx.clone())),
            s(&["/usr/bin/env", "/bin/codex", "login", "--device-auth"])
        );
        assert_eq!(quiet_login("codex", &s(&["login", "status"]), far, &mut vec![], cx.clone()), cx);
    }
}
