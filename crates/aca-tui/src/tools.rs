//! The `n` tool menu: shell always on top, then every account registered in config.toml (named after the account, Claude
//! first, then Codex; a family with no account at all is listed as itself), and last the named tools (`[tools.<name>]`).
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

use aca_core::config::Config;
use aca_core::t;

use crate::data::{Agent, agent_of_tool};

#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    /// Name shown in the menu
    pub name: String,
    /// aca's tool name (claude / codex / shell / a named one) and account (empty = this tool's default)
    pub tool: String,
    pub account: String,
    /// Which family, for coloring in the menu
    pub agent: Agent,
    /// The text after the name in the menu: a named tool shows its command; an account shows nothing (the menu puts the
    /// family and quota there)
    pub note: String,
    /// Why it can't be used (the tool's default account isn't registered), shown in the menu in place of the note
    pub problem: Option<String>,
}

#[cfg(test)]
impl Tool {
    /// The form passed to `aca new`: `tool` / `tool:account`
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

/// Read the menu (hidden ones left out)
pub fn load() -> Vec<Tool> {
    menu(&aca_core::config::load(), &Layout { menu: crate::config::list("ui", "menu"), hide: crate::config::list("ui", "hide") })
}

fn entry(cfg: &Config, name: String, tool: &str, account: &str, note: String) -> Tool {
    let problem = aca_core::agent::pick(cfg, &spec(tool, account)).err();
    Tool { name, tool: tool.into(), account: account.into(), agent: agent_of_tool(cfg, tool), note, problem }
}

/// A named tool: which family, what command, which args, which account
fn tool_note(name: &str, t: &aca_core::config::Tool) -> String {
    let cmd = if !t.cmd.is_empty() {
        t.cmd.iter().map(|c| crate::create::tilde(c)).collect::<Vec<_>>().join(" ")
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
fn menu_with(cfg: &Config, lay: &Layout, installed: fn(&str) -> bool) -> Vec<Tool> {
    let mut out: Vec<Tool> = vec![];
    // Registered accounts: in built-in order (Claude, Codex, pi, opencode), each family's by name
    for fam in aca_core::agent::kinds().filter(|k| *k != "shell") {
        let all = cfg.accounts.get(fam);
        for n in all.into_iter().flat_map(|m| m.keys()) {
            out.push(entry(cfg, n.clone(), fam, n, String::new()));
        }
        // A family with no account at all: list it as itself (with its own default config dir). Claude and Codex always;
        // the later-added families only when installed (or given their section in config.toml), so people without them
        // don't get unusable rows in the menu
        let prog = cfg.tool(fam).and_then(|t| t.cmd.first().cloned()).unwrap_or_else(|| fam.to_string());
        let wanted = matches!(fam, "claude" | "codex") || cfg.tools.contains_key(fam) || installed(&prog);
        if all.is_none_or(|m| m.is_empty()) && wanted {
            out.push(entry(cfg, fam.into(), fam, "", String::new()));
        }
    }
    // Named tools
    for (name, t) in cfg.tools.iter().filter(|(k, _)| aca_core::agent::builtin(k).is_none()) {
        out.push(entry(cfg, name.clone(), name, "", tool_note(name, t)));
    }
    out.retain(|t| !lay.hide.contains(&t.name));
    // Ones given an order go first (stable sort: the rest keep the order above)
    out.sort_by_key(|t| lay.menu.iter().position(|n| *n == t.name).unwrap_or(usize::MAX));
    let shell = Tool {
        name: "shell".into(),
        tool: "shell".into(),
        account: String::new(),
        agent: Agent::None,
        note: t!("仅打开终端", "just a terminal").into(),
        problem: None,
    };
    out.insert(0, shell);
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
    }

    #[test]
    fn a_family_without_accounts_is_listed_as_itself() {
        let only = aca_core::config::parse("[accounts.claude]\nteam = \"~/.ct\"\n");
        assert_eq!(names(&menu_with(&only, &Layout::default(), none)), ["shell", "team", "codex"]);
        assert_eq!(names(&menu_with(&Config::default(), &Layout::default(), none)), ["shell", "claude", "codex"]);
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
    fn the_layout_orders_and_hides() {
        let lay = Layout { menu: vec!["alt".into(), "mycx".into()], hide: vec!["aider".into(), "bad".into()] };
        assert_eq!(names(&menu_with(&cfg(), &lay, none)), ["shell", "alt", "mycx", "bed", "team"]);
    }
}
