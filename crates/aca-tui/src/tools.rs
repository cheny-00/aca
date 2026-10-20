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
    /// 菜单里名字后面那句:自己起名的工具写它的命令;账号不写(菜单在那写哪家和额度)
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
    // 测试里不看这台机器装了什么
    menu_with(cfg, lay, if cfg!(test) { |_| false } else { crate::accounts::on_path })
}

/// `installed` 问一个程序装了没有(测试里换掉)
fn menu_with(cfg: &Config, lay: &Layout, installed: fn(&str) -> bool) -> Vec<Tool> {
    let mut out: Vec<Tool> = vec![];
    // 登记的账号:照内置的先后(Claude、Codex、pi、opencode),各自按名字
    for fam in aca_core::agent::kinds().filter(|k| *k != "shell") {
        let all = cfg.accounts.get(fam);
        for n in all.into_iter().flat_map(|m| m.keys()) {
            out.push(entry(cfg, n.clone(), fam, n, String::new()));
        }
        // 哪家一个账号都没有:放它本身(用它自己默认的配置目录)。Claude、Codex 总在;
        // 后加的几家装了(或者 config.toml 里写了它那一段)才放,没装的人菜单里不多出用不了的几行
        let prog = cfg.tool(fam).and_then(|t| t.cmd.first().cloned()).unwrap_or_else(|| fam.to_string());
        let wanted = matches!(fam, "claude" | "codex") || cfg.tools.contains_key(fam) || installed(&prog);
        if all.is_none_or(|m| m.is_empty()) && wanted {
            out.push(entry(cfg, fam.into(), fam, "", String::new()));
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
        note: t!("只开终端", "just a terminal").into(),
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
        // 不换配置目录的账号(bed:只带环境变量)也在
        assert_eq!(names(&v), ["shell", "bed", "team", "alt", "aider", "bad", "mycx"]);
        assert_eq!(v[1].spec(), "claude:bed");
        assert_eq!((v[2].spec(), v[2].agent, v[2].note.as_str()), ("claude:team".to_string(), Agent::Claude, ""));
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
        // 自己定了命令的看那个命令装了没有
        let wrapped = aca_core::config::parse("[tools.pi]\ncmd = [\"/opt/pi/bin/pi\"]\n");
        assert_eq!(names(&menu_with(&wrapped, &Layout::default(), none)), ["shell", "claude", "codex", "pi"]);
    }

    #[test]
    fn the_layout_orders_and_hides() {
        let lay = Layout { menu: vec!["alt".into(), "mycx".into()], hide: vec!["aider".into(), "bad".into()] };
        assert_eq!(names(&menu_with(&cfg(), &lay, none)), ["shell", "alt", "mycx", "bed", "team"]);
    }
}
