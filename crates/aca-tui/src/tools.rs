//! `n` 的工具菜单:最上面总是 shell,然后是 config.toml 里登记的每个账号(名字就是账号名,先 Claude 再 Codex;
//! 哪家一个账号都没有就放它本身),最后是自己起名的工具(`[tools.<名>]`)。
//!
//! 顺序和藏哪些在 config.toml 里调(都可以不写):
//!
//! ```toml
//! [ui]
//! menu = ["work", "team"]            # 写了的按这个顺序排在前面,没写到的接在后面
//! hide = ["official"]                # 菜单里不列(`aca new claude:official` 照样能用)
//! ```
//!
//! 要带参数的做成一个工具(`[tools.ccx] kind = "claude"`、`args = […]`、`account = "sub"`),见 aca-core 的 config.rs。

use aca_core::config::Config;
use aca_core::t;

use crate::data::{Agent, agent_of_tool};

#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    /// 菜单里显示的名字
    pub name: String,
    /// aca 的工具名(claude / codex / shell / 自己起名的)和账号(空 = 这个工具默认的)
    pub tool: String,
    pub account: String,
    /// 哪家,菜单里上色用
    pub agent: Agent,
    /// 菜单里名字后面那句:账号的配置目录、工具的命令
    pub note: String,
    /// 用不了的原因(工具写的默认账号没登记),菜单里代替说明显示
    pub problem: Option<String>,
}

#[cfg(test)]
impl Tool {
    /// 交给 `aca new` 的写法:`工具` / `工具:账号`
    pub fn spec(&self) -> String {
        spec(&self.tool, &self.account)
    }
}

pub fn spec(tool: &str, account: &str) -> String {
    if account.is_empty() { tool.to_string() } else { format!("{tool}:{account}") }
}

/// 菜单的顺序和藏起来的(config.toml 的 `[ui]`)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub menu: Vec<String>,
    pub hide: Vec<String>,
}

/// 读菜单(藏起来的不要)
pub fn load() -> Vec<Tool> {
    menu(&aca_core::config::load(), &Layout { menu: crate::config::list("ui", "menu"), hide: crate::config::list("ui", "hide") })
}

fn entry(cfg: &Config, name: String, tool: &str, account: &str, note: String) -> Tool {
    let problem = aca_core::agent::pick(cfg, &spec(tool, account)).err();
    Tool { name, tool: tool.into(), account: account.into(), agent: agent_of_tool(cfg, tool), note, problem }
}

/// 账号那一层带了什么:配置目录;不换目录的写它带的是环境变量 / 参数
fn account_note(a: &aca_core::config::Account) -> String {
    if !a.home.is_empty() {
        return crate::create::tilde(&a.home);
    }
    let mut parts = vec![];
    if !a.env.is_empty() || !a.env_file.is_empty() {
        parts.push(t!("环境变量", "env vars").to_string());
    }
    if !a.args.is_empty() {
        parts.push(a.args.join(" "));
    }
    parts.join(" · ")
}

/// 自己起名的工具:哪一家、什么命令、带什么参数、用哪个账号
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
    let mut out: Vec<Tool> = vec![];
    // 登记的账号:先 Claude 再 Codex,各自按名字
    for fam in ["claude", "codex"] {
        let all = cfg.accounts.get(fam);
        for (n, a) in all.into_iter().flatten() {
            out.push(entry(cfg, n.clone(), fam, n, account_note(a)));
        }
        // 哪家一个账号都没有:放它本身(用它自己默认的配置目录)
        if all.is_none_or(|m| m.is_empty()) {
            out.push(entry(cfg, fam.into(), fam, "", t!("它自己默认的配置目录", "its own default config directory").into()));
        }
    }
    // 自己起名的工具
    for (name, t) in cfg.tools.iter().filter(|(k, _)| aca_core::agent::builtin(k).is_none()) {
        out.push(entry(cfg, name.clone(), name, "", tool_note(name, t)));
    }
    out.retain(|t| !lay.hide.contains(&t.name));
    // 写了顺序的排前面(稳定排序:没写到的保持上面的先后)
    out.sort_by_key(|t| lay.menu.iter().position(|n| *n == t.name).unwrap_or(usize::MAX));
    let shell = Tool {
        name: "shell".into(),
        tool: "shell".into(),
        account: String::new(),
        agent: Agent::None,
        note: t!("开个 shell,自己起", "open a shell, start it yourself").into(),
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

    #[test]
    fn accounts_are_listed_then_named_tools_and_shell_comes_first() {
        let v = menu(&cfg(), &Layout::default());
        // 不换配置目录的账号(bed:只带环境变量)也在
        assert_eq!(names(&v), ["shell", "bed", "team", "alt", "aider", "bad", "mycx"]);
        assert_eq!((v[1].spec(), v[1].note.as_str()), ("claude:bed".to_string(), "环境变量"));
        assert_eq!((v[2].spec(), v[2].agent), ("claude:team".to_string(), Agent::Claude));
        assert!(v[2].note.ends_with(".ct"), "{}", v[2].note);
        assert_eq!((v[3].spec(), v[3].agent), ("codex:alt".to_string(), Agent::Codex));
        // 自己起名的工具认 kind;声明式的算别家
        assert_eq!((v[6].spec(), v[6].agent, v[6].note.as_str()), ("mycx".to_string(), Agent::Codex, "codex · alt"));
        assert_eq!((v[4].agent, v[4].note.as_str()), (Agent::Other, "aider"));
        // 工具写的默认账号没登记:标出来
        assert!(v[5].problem.as_ref().is_some_and(|p| p.contains("nope")), "{:?}", v[5].problem);
        assert!(v.iter().enumerate().all(|(i, t)| i == 5 || t.problem.is_none()));
    }

    #[test]
    fn a_family_without_accounts_is_listed_as_itself() {
        let only = aca_core::config::parse("[accounts.claude]\nteam = \"~/.ct\"\n");
        assert_eq!(names(&menu(&only, &Layout::default())), ["shell", "team", "codex"]);
        assert_eq!(names(&menu(&Config::default(), &Layout::default())), ["shell", "claude", "codex"]);
    }

    #[test]
    fn the_layout_orders_and_hides() {
        let lay = Layout { menu: vec!["alt".into(), "mycx".into()], hide: vec!["aider".into(), "bad".into()] };
        assert_eq!(names(&menu(&cfg(), &lay)), ["shell", "alt", "mycx", "bed", "team"]);
    }
}
