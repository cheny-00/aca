//! Which vendor's agent a session runs, as far as reading its records goes: each vendor keeps them in its own place and shape.

use aca_core::config::Config;
use aca_core::proc::{self, Kids};
use aca_core::store;
use aca_core::tmux::Pane;

/// Which vendor's agent runs in the session. The account name in the list is colored by it
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Agent {
    /// A plain tmux session, or one opened as a shell in the first place
    #[default]
    None,
    Claude,
    Codex,
    Pi,
    Opencode,
    Agy,
    /// A declared tool (a `[tools.<name>]` in config.toml without a kind)
    Other,
}

pub fn kind_of(name: &str) -> Agent {
    match name {
        "" | "shell" => Agent::None,
        "claude" => Agent::Claude,
        "codex" => Agent::Codex,
        "pi" => Agent::Pi,
        "opencode" => Agent::Opencode,
        "agy" => Agent::Agy,
        _ => Agent::Other,
    }
}

/// Which vendor a tool name is: claude / codex themselves, a custom-named tool by its `kind`, declared tools count as others
pub fn agent_of_tool(cfg: &Config, tool: &str) -> Agent {
    match aca_core::agent::tool(cfg, tool) {
        Some(t) if !t.kind.is_empty() => kind_of(&t.kind),
        Some(_) => Agent::Other,
        None => kind_of(tool),
    }
}

/// The built-in tool this vendor is; None for a shell and declared tools
pub fn builtin(agent: Agent) -> Option<&'static dyn aca_core::agent::Agent> {
    let kind = match agent {
        Agent::Claude => "claude",
        Agent::Codex => "codex",
        Agent::Pi => "pi",
        Agent::Opencode => "opencode",
        Agent::Agy => "agy",
        Agent::None | Agent::Other => return None,
    };
    aca_core::agent::builtin(kind)
}

/// Which environment variable takes this vendor's config dir; None where there's no such thing (shell, declared tools, opencode, agy)
pub fn home_var(agent: Agent) -> Option<&'static str> {
    builtin(agent)?.home_var()
}

/// Config dir the agent process uses (CLAUDE_CONFIG_DIR / CODEX_HOME / PI_CODING_AGENT_DIR in its environment);
/// unset or no such thing = None (use the default)
pub fn config_home(pid: u32, agent: Agent) -> Option<String> {
    let d = aca_core::proc::env_var(pid, home_var(agent)?)?;
    Some(aca_core::paths::expand(&d).trim_end_matches('/').to_string())
}

/// How the account is written in the list: the registered name; one given as a directory is the directory name
/// (leading dot removed)
fn account_name(account: &str) -> String {
    aca_core::agent::account_label(account).trim_start_matches('.').to_string()
}

/// A registered session: which vendor, which account (the tool name if no account)
pub fn registered(cfg: &Config, s: &store::Session) -> (Agent, String) {
    let agent = agent_of_tool(cfg, &s.tool);
    if agent == Agent::None {
        return (Agent::None, String::new());
    }
    let who = if s.account.is_empty() { s.tool.clone() } else { account_name(&s.account) };
    (agent, who)
}

/// The agent running in the pane (started by hand in a shell, or the pane opened straight into it): (claude / codex, pid)
pub fn agent_in(kids: &Kids, p: &Pane) -> Option<(&'static str, u32)> {
    if p.pid == 0 {
        return None;
    }
    proc::agent_under(kids, p.pid).or_else(|| aca_core::agent::by_comm(&p.cmd).map(|a| (a, p.pid)))
}

/// Accounts with a config dir of their own: (vendor, name, dir). A registered `home`, or for one bound to a sandbox the dir it
/// uses there (`<workspace>/<kind>`, mounted at the same path, so its records are read from this Mac too). Claude first, then
/// Codex, each by name
pub fn accounts(cfg: &Config) -> Vec<(Agent, String, String)> {
    let dir = |kind: &str, a: &aca_core::config::Account| match cfg.sandbox.get(&a.sandbox) {
        Some(b) if a.home.is_empty() && !b.workspace.trim().is_empty() => Some(b.home(kind).to_string_lossy().into_owned()),
        _ => Some(a.home.clone()).filter(|h| !h.is_empty()),
    };
    let mut v: Vec<(Agent, String, String)> = cfg
        .accounts
        .iter()
        .flat_map(|(kind, m)| m.iter().filter_map(move |(n, a)| Some((kind_of(kind), n.clone(), dir(kind, a)?))))
        .filter(|(a, _, _)| matches!(a, Agent::Claude | Agent::Codex))
        .collect();
    v.sort_by_key(|s| s.0 != Agent::Claude);
    v
}
