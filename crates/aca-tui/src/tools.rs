//! The `n` tool menu: shell first by default; then every account registered in config.toml (named after the account, Claude
//! first, then Codex; a family with no account at all is listed as itself), and the named tools (`[tools.<name>]`).
//!
//! The special environments' rows (`[sandbox.<name>]`, see aca-core's sandbox.rs) come last in [`load`] and have a panel of
//! their own (`^E`): each environment's together, a terminal in it (`shell:<name>`) then the accounts bound to it, under a
//! heading that says what it is (`ssh work`, `本机 · Los_Angeles · 代理 127.0.0.1:28888`).
//!
//! Order and what to hide are set in config.toml (both optional):
//!
//! ```toml
//! [ui]
//! menu = ["work", "team"]            # listed ones go first in this order, the rest follow
//! hide = ["official"]                # not listed in the menu (`aca new claude:official` still works)
//! ```
//!
//! One that needs args is made a tool (`[tools.ccx] kind = "claude"`, `args = […]`, `account = "sub"`), see aca-core's
//! config.rs.

use aca_core::config::{Config, Sandbox};
use aca_core::{t, tr};
use aca_records::agent::{Agent, agent_of_tool};

#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    /// Name shown in the menu
    pub name: String,
    /// aca's tool name (claude / codex / shell / a named one) and account (empty = this tool's default)
    pub tool: String,
    pub account: String,
    /// Which family, for coloring in the menu
    pub agent: Agent,
    /// The text after the name in the menu: a named tool shows its command, a terminal says so; an account shows nothing (the
    /// menu puts the family and quota there)
    pub note: String,
    /// Why it can't be used (the tool's default account isn't registered), shown in the menu in place of the note
    pub problem: Option<String>,
    /// The special environment it runs in (`[sandbox.<it>]`); empty = this Mac as it is. Non-empty = one of `^E`'s rows
    pub within: String,
    /// What that environment is ([`under`]), for the heading of its rows
    pub under: String,
}

impl Tool {
    pub(crate) fn google_login(&self) -> bool {
        self.agent == Agent::Agy && self.tool == "agy" && self.account.is_empty()
    }

    pub(crate) fn has_login(&self) -> bool {
        self.google_login() || (!self.account.is_empty() && matches!(self.agent, Agent::Claude | Agent::Codex))
    }

    /// The form passed to `aca new`: `tool` / `tool:account`
    #[cfg(test)]
    pub fn spec(&self) -> String {
        spec(&self.tool, &self.account)
    }
}

pub fn spec(tool: &str, account: &str) -> String {
    if account.is_empty() { tool.to_string() } else { format!("{tool}:{account}") }
}

/// Menu order and hidden entries (config.toml's `[ui]`)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub menu: Vec<String>,
    pub hide: Vec<String>,
}

/// Read the menu (hidden ones left out): this Mac's, then the special environments' (X picks from all of them)
pub fn load() -> Vec<Tool> {
    menu(&aca_core::config::load(), &Layout { menu: crate::config::list("ui", "menu"), hide: crate::config::list("ui", "hide") })
}

/// The menu split as the interface shows it: `n`'s rows, and `^E`'s (the special environments')
pub fn split(v: Vec<Tool>) -> (Vec<Tool>, Vec<Tool>) {
    v.into_iter().partition(|t| t.within.is_empty())
}

fn entry(cfg: &Config, name: String, tool: &str, account: &str, note: String) -> Tool {
    let problem = aca_core::agent::pick(cfg, &spec(tool, account)).err();
    let (within, under) = match aca_core::sandbox::of(cfg, tool, account) {
        Some((n, b)) => (n.to_string(), self::under(b)),
        None => (String::new(), String::new()),
    };
    Tool { name, tool: tool.into(), account: account.into(), agent: agent_of_tool(cfg, tool), note, problem, within, under }
}

/// What a special environment is: where it runs (`ssh work`, or `本机` (this Mac)), then the time zone and the proxy its env
/// sets (`Los_Angeles · 代理 127.0.0.1:28888`)
pub fn under(b: &Sandbox) -> String {
    let mut parts = vec![if b.cage() { format!("ssh {}", b.ssh.trim()) } else { t!("本机", "this Mac").to_string() }];
    // Either case counts (curl reads only the lowercase http_proxy); empty = not set
    let set = |names: &[&str]| {
        names.iter().find_map(|n| {
            b.env.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.trim().to_string()).filter(|v| !v.is_empty())
        })
    };
    if let Some(tz) = set(&["TZ"]) {
        parts.push(tz.trim_start_matches(':').rsplit('/').next().unwrap_or_default().to_string());
    }
    if let Some(p) = set(&["HTTPS_PROXY", "ALL_PROXY", "HTTP_PROXY"]) {
        parts.push(tr!("代理 {}", "proxy {}", proxy_addr(&p)));
    }
    parts.join(" · ")
}

/// A proxy URL as its address: `http(s)://` dropped, any other scheme kept (`socks5h://…`), and never the `user:password@`
fn proxy_addr(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let addr = rest.rsplit_once('@').map_or(rest, |(_, a)| a).trim_end_matches('/');
    if scheme.is_empty() || scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
        addr.to_string()
    } else {
        format!("{scheme}://{addr}")
    }
}

/// A named tool: which family, what command, which args, which account
fn tool_note(name: &str, t: &aca_core::config::Tool) -> String {
    let cmd = if !t.cmd.is_empty() {
        t.cmd.iter().map(|c| aca_records::words::tilde(c)).collect::<Vec<_>>().join(" ")
    } else if !t.kind.is_empty() {
        t.kind.clone()
    } else {
        name.to_string()
    };
    let mut parts = vec![cmd];
    parts.extend(t.args.iter().cloned());
    if !t.account.is_empty() {
        parts.push(format!("· {}", aca_core::agent::account_label(&t.account)));
    }
    parts.join(" ")
}

pub fn menu(cfg: &Config, lay: &Layout) -> Vec<Tool> {
    // Tests don't look at what this machine has installed
    menu_with(cfg, lay, if cfg!(test) { |_| false } else { crate::accounts::on_path })
}

/// `installed` asks whether a program is installed (swapped out in tests)
pub(crate) fn menu_with(cfg: &Config, lay: &Layout, installed: fn(&str) -> bool) -> Vec<Tool> {
    let families = || aca_core::agent::kinds().filter(|k| *k != "shell");
    let agy = aca_core::agent::tool(cfg, "agy").unwrap_or_default();
    let has_agy = installed(agy.cmd.first().map_or("agy", String::as_str));
    let bound = |fam: &str, account: &str| aca_core::sandbox::of(cfg, fam, account).map(|(n, _)| n.to_string());
    // The special environments': each one's together, a terminal in it then the accounts bound to it in built-in order
    let mut special: Vec<Tool> = vec![];
    for name in cfg.sandbox.keys() {
        special.push(entry(cfg, format!("shell:{name}"), "shell", name, t!("终端", "terminal").into()));
        for fam in families() {
            for n in cfg.accounts.get(fam).into_iter().flat_map(|m| m.keys()).filter(|n| bound(fam, n).as_ref() == Some(name)) {
                special.push(entry(cfg, n.clone(), fam, n, String::new()));
            }
        }
    }
    let mut out: Vec<Tool> = vec![];
    // The other registered accounts: in built-in order (Claude, Codex, pi, opencode, agy), each family's by name
    for fam in families() {
        let all = cfg.accounts.get(fam);
        for n in all.into_iter().flat_map(|m| m.keys()).filter(|n| bound(fam, n).is_none()) {
            out.push(entry(cfg, n.clone(), fam, n, String::new()));
        }
        // A family with no account at all: list it as itself (with its own default config dir). Claude and Codex always;
        // the later-added families only when installed (or given their section in config.toml), so people without them
        // don't get unusable rows in the menu
        let prog = aca_core::agent::tool(cfg, fam).and_then(|t| t.cmd.first().cloned()).unwrap_or_else(|| fam.to_string());
        let wanted = matches!(fam, "claude" | "codex") || cfg.tools.contains_key(fam) || installed(&prog);
        if (all.is_none_or(|m| m.is_empty()) || (fam == "agy" && agy.account.is_empty())) && wanted {
            out.push(entry(cfg, fam.into(), fam, "", String::new()));
        }
    }
    // Named tools
    for (name, t) in cfg.tools.iter().filter(|(k, _)| aca_core::agent::builtin(k).is_none()) {
        out.push(entry(cfg, name.clone(), name, "", tool_note(name, t)));
    }
    out.retain(|t| !lay.hide.contains(&t.name));
    special.retain(|t| !lay.hide.contains(&t.name));
    let shell = Tool {
        name: "shell".into(),
        tool: "shell".into(),
        account: String::new(),
        agent: Agent::None,
        note: t!("仅打开终端", "just a terminal").into(),
        problem: None,
        within: String::new(),
        under: String::new(),
    };
    out.insert(0, shell);
    // Legacy menus left shell implicit at the top. Once listed, it sorts like every other name.
    out.sort_by_key(|t| match lay.menu.iter().position(|n| *n == t.name) {
        Some(i) => i + 1,
        None if t.tool == "shell" => 0,
        None => usize::MAX,
    });
    out.extend(special);
    out.retain(|t| t.agent != Agent::Agy || has_agy);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        aca_core::config::parse(
            "[accounts.claude]\nteam = \"~/.ct\"\nbed = { env = { X = \"1\" } }\n[accounts.codex]\nalt = \"~/.cx\"\n\
             [tools.mycx]\nkind = \"codex\"\naccount = \"alt\"\n[tools.aider]\n[tools.bad]\nkind = \"claude\"\naccount = \"nope\"\n",
        )
    }

    fn names(v: &[Tool]) -> Vec<&str> {
        v.iter().map(|t| t.name.as_str()).collect()
    }

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn agy_menu_keeps_the_google_login_separate_from_configured_variants() {
        let cfg = aca_core::config::parse("[tools.agy]\nargs = ['--mode', 'plan']\n[accounts.agy]\napi = { args = ['--other'] }");
        let menu = menu_with(&cfg, &Layout::default(), |p| p == "agy");
        assert_eq!(menu.iter().filter(|t| t.agent == Agent::Agy).map(Tool::spec).collect::<Vec<_>>(), ["agy:api", "agy"]);
        assert!(menu_with(&cfg, &Layout::default(), none).iter().all(|t| t.agent != Agent::Agy));
    }

    #[test]
    fn accounts_are_listed_then_named_tools_and_shell_comes_first() {
        let v = menu_with(&cfg(), &Layout::default(), none);
        // An account that doesn't change the config dir (bed: env vars only) is there too
        assert_eq!(names(&v), ["shell", "bed", "team", "alt", "aider", "bad", "mycx"]);
        assert_eq!(v[1].spec(), "claude:bed");
        assert_eq!((v[2].spec(), v[2].agent, v[2].note.as_str()), ("claude:team".to_string(), Agent::Claude, ""));
        assert_eq!((v[3].spec(), v[3].agent), ("codex:alt".to_string(), Agent::Codex));
        // A named tool goes by its kind; a declared tool counts as Other
        assert_eq!((v[6].spec(), v[6].agent, v[6].note.as_str()), ("mycx".to_string(), Agent::Codex, "codex · alt"));
        assert_eq!((v[4].agent, v[4].note.as_str()), (Agent::Other, "aider"));
        // The tool's default account isn't registered: flagged
        assert!(v[5].problem.as_ref().is_some_and(|p| p.contains("nope")), "{:?}", v[5].problem);
        assert!(v.iter().enumerate().all(|(i, t)| i == 5 || t.problem.is_none()));
        // No special environment: all of them are n's
        assert!(split(v).1.is_empty());
    }

    #[test]
    fn a_family_without_accounts_is_listed_as_itself() {
        let only = aca_core::config::parse("[accounts.claude]\nteam = \"~/.ct\"\n");
        assert_eq!(names(&menu_with(&only, &Layout::default(), none)), ["shell", "team", "codex"]);
        assert_eq!(names(&menu_with(&Config::default(), &Layout::default(), none)), ["shell", "claude", "codex"]);
    }

    #[test]
    fn antigravity_is_offered_when_agy_is_installed() {
        let menu = menu_with(&Config::default(), &Layout::default(), |p| p == "agy");
        assert_eq!(names(&menu), ["shell", "claude", "codex", "agy"]);
        assert_eq!(menu[3].agent, Agent::Agy);
        assert_eq!(aca_records::words::agent_label(menu[3].agent, &menu[3].name), "Antigravity");
    }

    #[test]
    fn pi_and_opencode_show_up_when_installed_configured_or_given_accounts() {
        fn pi_only(p: &str) -> bool {
            p == "pi"
        }
        assert_eq!(names(&menu_with(&Config::default(), &Layout::default(), pi_only)), ["shell", "claude", "codex", "pi"]);
        let cfg = aca_core::config::parse("[accounts.pi]\nwork = \"~/.pi-work\"\n[tools.opencode]\nargs = [\"--agent\", \"plan\"]\n");
        let v = menu_with(&cfg, &Layout::default(), none);
        assert_eq!(names(&v), ["shell", "claude", "codex", "work", "opencode"]);
        assert_eq!((v[3].spec(), v[3].agent, v[4].agent), ("pi:work".to_string(), Agent::Pi, Agent::Opencode));
        // One with its own cmd: whether that command is installed
        let wrapped = aca_core::config::parse("[tools.pi]\ncmd = [\"/opt/pi/bin/pi\"]\n");
        assert_eq!(names(&menu_with(&wrapped, &Layout::default(), none)), ["shell", "claude", "codex", "pi"]);
    }

    #[test]
    fn each_special_environment_comes_last_with_a_terminal_in_it_and_its_accounts() {
        let cfg = aca_core::config::parse(
            "[sandbox.work]\nssh = \"work\"\n\
             [sandbox.unis]\nworkspace = \"/w/unis\"\n\
             env = { TZ = \"America/Los_Angeles\", https_proxy = \"http://me:secret@127.0.0.1:28888/\" }\n\
             [accounts.claude]\nsub = \"~/.c\"\nteam = { sandbox = \"work\" }\n\
             [accounts.codex]\ncx = \"~/.cx\"\nteam = { sandbox = \"work\" }\nteam-unis = { sandbox = \"unis\" }\n\
             [accounts.shell]\nplain = { env = { X = \"1\" } }\n",
        );
        let v = menu_with(&cfg, &Layout::default(), none);
        // This Mac's first; each environment's together after (by name), a terminal in it first
        assert_eq!(names(&v), ["shell", "sub", "cx", "shell:unis", "team-unis", "shell:work", "team", "team"]);
        let rows: Vec<_> = v[3..].iter().map(|t| (t.spec(), t.within.as_str(), t.under.as_str())).collect();
        // What each environment is, never the proxy's password
        let la = "本机 · Los_Angeles · 代理 127.0.0.1:28888";
        assert_eq!(
            rows,
            [
                ("shell:unis".into(), "unis", la),
                ("codex:team-unis".into(), "unis", la),
                ("shell:work".into(), "work", "ssh work"),
                ("claude:team".into(), "work", "ssh work"),
                ("codex:team".into(), "work", "ssh work"),
            ]
        );
        assert!(v[..3].iter().all(|t| t.within.is_empty()) && v.iter().all(|t| t.problem.is_none()));
        assert_eq!((v[3].agent, v[3].note.as_str()), (Agent::None, "终端"));
        // A registered terminal in no environment isn't listed
        assert!(!v.iter().any(|t| t.name == "shell:plain"));
        // An order in [ui] doesn't move them; hiding still works
        let lay = Layout { menu: vec!["cx".into(), "sub".into()], hide: vec![] };
        assert_eq!(names(&menu_with(&cfg, &lay, none))[..3], ["shell", "cx", "sub"]);
        let lay = Layout { menu: vec![], hide: vec!["shell:work".into()] };
        assert!(!names(&menu_with(&cfg, &lay, none)).contains(&"shell:work"));
        // As the interface shows them: n's, and ^E's
        let (n, e) = split(v);
        assert_eq!((names(&n), e.len()), (vec!["shell", "sub", "cx"], 5));
    }

    #[test]
    fn the_layout_orders_and_hides() {
        let lay = Layout { menu: vec!["alt".into(), "mycx".into()], hide: vec!["aider".into(), "bad".into()] };
        assert_eq!(names(&menu_with(&cfg(), &lay, none)), ["shell", "alt", "mycx", "bed", "team"]);
    }
}
