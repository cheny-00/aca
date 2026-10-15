//! `~/.config/aca/config.toml`:账号和工具。都是可选的,一个字不写也能用(claude / codex 用各自默认的配置目录)。
//!
//! ```toml
//! [accounts.claude]            # 账号 = 起它时多带的那几样,用它自己的词写。只换配置目录的写一个字符串就行
//! work = "~/.claude-work"
//! personal = "~/.claude-personal"
//! bedrock = { env = { CLAUDE_CODE_USE_BEDROCK = "1", AWS_PROFILE = "work" } }
//!
//! [accounts.codex]
//! work = "~/.codex-work"
//! team = { home = "~/.codex2", args = ["--profile", "team"] } # 目录之外还要带参数
//! api = { env_file = "~/.secrets/openai.env" }                # 不换目录,只换 key
//!
//! [tools.claude]               # 内置工具的默认值
//! account = "work"             # 没指定账号时用哪个
//! args = ["--dangerously-skip-permissions"]
//! mod = true                   # 起它时带上 aca 的 mod:订阅额度不靠状态栏(不写就不带;接口还早,见 agent/claude_mod.rs)
//!
//! [tools.ccx]                  # 自己起名字的工具:`aca new ccx`
//! kind = "claude"              # 按内置的哪一家起、接 hook(claude / codex / shell)
//! cmd = ["claude"]             # 不写就是那一家的命令
//! args = ["--model", "opus"]
//! account = "personal"
//! env = { ANTHROPIC_BASE_URL = "https://…" }   # 起的时候加的环境变量,哪种工具都能写
//!
//! [tools.codex]
//! hook_trust = "bypass"        # 不写:Codex 每个账号问一次「Hooks need review」,点「Trust all」之后不再问
//!
//! [inbox]                      # 会话之间递话
//! notify = ["done", "failed"]  # worker 在哪些事上通知派它的那个;all = 都要,不写 = 都不通知
//!
//! [trust]
//! folder = true                # 起会话时替你答「信不信任这个目录」(Claude、Codex 头一回进一个目录都问);不写 = 不替你答
//!
//! [idle]                       # 闲着的会话
//! stop_after = "3h"            # 空闲这么久就停掉(agent 退出,窗格留着,列表里标 ☾;⏎ / aca start 接着原来的对话再起)。
//!                              # 写成 30m、3h、1d、1h30m;不写 = 不自动停。aca 界面开着时每 5 分钟看一次;
//!                              # 这一轮没跑完的、有人连着的、有后台任务的等等不停(见 session::stop_idle)
//!
//! [keys]                       # aca 自己绑在 tmux 上的键(不用前缀,只在 aca 的会话里拦;不写进你的 tmux.conf)
//! next = "C-M-n"               # 进下一个在等你的会话(`aca next`)。不写就是它;"" = 不绑
//!
//! [login]                      # aca 起的登录(账号页的 ⏎、aca run … login):不替你开默认浏览器,链接弹出来挑
//! browsers = ["Firefox", { name = "Chrome 工作", cmd = ["google-chrome", "--profile-directory=Profile 1"] }]
//!                              # 不写:自动找(Mac 看 .app,Linux 看 .desktop)。字符串:Mac 上是 App 的名字,Linux 上是命令;
//!                              # 表:名字 + 命令,链接接在最后(参数里写 {url} 就换在那)
//! remote = "auto"              # 当不当远程(远程:不列浏览器,换成在别的设备上登得完的):auto 看 SSH、Linux 有没有图形界面;yes / no 定死
//!
//! [tools.aider]                # 不写 kind、名字也不是内置的:声明式工具,下面几项告诉 aca 怎么对待它(见 agent/declared.rs)
//! cmd = ["aider"]              # 不写就是工具名
//! home_var = "AIDER_HOME"      # 账号的 home 交给哪个环境变量;不写的话账号里只能用 env / args
//! resume = ["--session", "{conv}"]   # 接着对话怎么起;{conv} 换成对话 id(id 由 hook 报上来)
//! fork = ["--fork", "{conv}"]
//! busy = ['Working\.\.\.']     # 屏幕上出现就算在跑(正则)
//! ask = ['\(y/n\)']            # 屏幕上出现就算在等你(正则)
//! title = "pane"               # 标题取窗格标题;不写就只用 hook 报的
//! ```

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{agent, paths};

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Tool {
    /// 内置的哪一家;空 = 声明式工具
    pub kind: String,
    pub cmd: Vec<String>,
    pub args: Vec<String>,
    pub account: String,
    pub env: BTreeMap<String, String>,
    /// Codex:`"bypass"` = 启动时加 `--dangerously-bypass-hook-trust`,不让它问「Hooks need review」
    pub hook_trust: String,
    /// Claude:`true` = 起它时带上 aca 的 mod(`--plugin-dir`),订阅额度从它那来、不靠状态栏。见 agent/claude_mod.rs
    #[serde(rename = "mod")]
    pub mods: bool,
    // 下面几项只给声明式工具用;内置的几家各有各的办法,写了也不看
    pub home_var: String,
    pub resume: Vec<String>,
    pub fork: Vec<String>,
    pub busy: Vec<String>,
    pub ask: Vec<String>,
    pub title: String,
    /// 这一次启动要带上的、开着的 aca 插件给这一家的插件目录(见 [`crate::plugin::agent_dirs`])。启动时填,配置文件里不写
    #[serde(skip)]
    pub plugin_dirs: Vec<String>,
    /// 这一次启动要直接挂的 MCP 服务(见 [`crate::plugin::mount`]):(名字, 交给 aca 的参数)。启动时填
    #[serde(skip)]
    pub mcp: Vec<(String, Vec<String>)>,
    /// Codex:这一次要打开的 aca 插件(`-c plugins.<名字>@aca.enabled=true`)。启动时填
    #[serde(skip)]
    pub codex_plugins: Vec<String>,
}

/// 账号:起 agent 时在工具上面再叠的一层。认证是 harness 自己的事(它的配置目录、它的 login、它的 key),
/// aca 只负责每次都把这几样原样带上,不知道也不管里面是订阅、API key 还是 Codex 的 profile
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(from = "AccountDef")]
pub struct Account {
    /// 配置目录(完整路径):交给这一家认的那个环境变量(`CLAUDE_CONFIG_DIR` / `CODEX_HOME` / 声明式工具的 `home_var`)
    pub home: String,
    pub env: BTreeMap<String, String>,
    /// 一行一个 `KEY=值` 的文件,每次启动时现读(放 key 的地方;aca 不存里面的东西)
    pub env_file: String,
    /// 每次都带的参数,比如 Codex 的 `--profile team`
    pub args: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AccountDef {
    /// 简写:只换配置目录
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

/// 配置里写的目录:展开 ~,末尾的 / 去掉
pub(crate) fn dir(p: &str) -> String {
    let d = paths::expand(p.trim());
    if d.len() > 1 { d.trim_end_matches('/').to_string() } else { d }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// 哪一家 → 账号名 → 那一层
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

/// `[idle]`:闲着的会话(见 [`crate::session::stop_idle`])
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Idle {
    /// 空闲这么久就停掉;原样留着,用的时候由 [`Idle::stop_after`] 认
    pub stop_after: Option<toml::Value>,
}

impl Idle {
    /// 空闲几秒就停;不自动停 = `Ok(None)`,写得认不出来 = Err 说为什么(这时也不停)
    pub fn stop_after(&self) -> Result<Option<u64>, String> {
        match &self.stop_after {
            None => Ok(None),
            Some(toml::Value::String(s)) => span(s).map(|n| (n > 0).then_some(n)).ok_or_else(|| {
                crate::tr!(
                    "stop_after 写成 30m、3h、1d、1h30m 这样,不认 \"{s}\"",
                    "write stop_after like 30m, 3h, 1d or 1h30m, not \"{s}\""
                )
            }),
            Some(v) => Err(crate::tr!(
                "stop_after 要写成带单位的字符串,比如 \"3h\",不认 {v}",
                "stop_after takes a string with a unit, like \"3h\", not {v}"
            )),
        }
    }
}

/// 时长:一个或几个「数 + 单位」连着写,单位 s / m / h / d(`90m`、`1h30m`)。空的 = 0;认不出来 = None
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
    // 最后一个数没写单位
    num.is_none().then_some(total)
}

/// `[login]`:aca 起的登录,链接怎么交给你(见 `agent::quiet_login`、界面的 browser.rs)
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Login {
    /// 链接弹出来时列哪些浏览器;不写 = 自动找,写了就只列这些
    pub browsers: Vec<Browser>,
    pub remote: Remote,
}

/// 一个浏览器:表上写的名字,和开链接的命令(链接接在最后;参数里写了 `{url}` 就换在那)
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(from = "BrowserDef")]
pub struct Browser {
    pub name: String,
    pub cmd: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BrowserDef {
    /// 简写:Mac 上是 App 的名字(`open -a`),别处是命令
    Short(String),
    Full {
        #[serde(default)]
        name: String,
        cmd: Vec<String>,
    },
}

impl From<BrowserDef> for Browser {
    fn from(d: BrowserDef) -> Browser {
        // 写的是路径时,表上只写文件名(`/opt/x/firefox` → firefox,`/Applications/Arc.app` → Arc)
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

/// `[login] remote`:当不当远程(见 `agent::remote`)。`"auto"`(不写)/ `"yes"` / `"no"`,也认 true / false
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
        Err(crate::tr!("remote 认 auto / yes / no,不认 {v}", "remote takes auto / yes / no, not {v}"))
    }
}

/// `[login]`、`[idle]` 写错了只当它没写,不连累别的(整个文件写坏才当空的);错在哪由 [`login_problem`]、[`idle_problem_in`] 说
fn lenient<'de, D: serde::Deserializer<'de>, T: serde::de::DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    Ok(toml::Value::deserialize(d)?.try_into().unwrap_or_default())
}

/// 不写 `[keys] next` 时的键:Ctrl+Alt+N
pub const NEXT_KEY: &str = "C-M-n";

/// `[keys]`:aca 自己绑在 tmux 上的键,和 Ctrl+Q、插件的热键一个办法(见 [`crate::tmux::bind_hotkeys`])
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Keys {
    /// 进下一个在等你的会话(`aca next`):tmux 的键名。不写 = [`NEXT_KEY`],`""` = 不绑
    pub next: Option<String>,
}

impl Keys {
    /// 绑哪个键;不要 = `Ok(None)`。写得不能当热键的不绑,Err 说为什么
    pub fn next_key(&self) -> Result<Option<String>, String> {
        let k = self.next.as_deref().unwrap_or(NEXT_KEY).trim();
        if k.is_empty() {
            return Ok(None);
        }
        crate::plugin::hotkey_ok(k).map(|_| Some(k.to_string()))
    }
}

/// `[trust]`:agent 启动时问的那几道「信不信任」
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Trust {
    /// `true` = 起会话之后屏幕上出现「信不信任这个目录」就替你选信任(见 [`crate::trust`])。
    /// 那个目录自己带的配置(hook、MCP 服务)从此不再问就能跑:只在你自己挑目录的机器上开
    pub folder: bool,
}

/// `[inbox]`:会话之间递话
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Inbox {
    /// worker 在哪些事上通知派它的那个:done(跑完一轮)/ failed(出错)/ exited(退出)/ waiting(在等确认)。
    /// `all` = 四样都要;不写 = 都不通知(不开口就不叫醒谁);`aca new --notify` 给一个会话单独定
    pub notify: Vec<String>,
}

pub fn load() -> Config {
    std::fs::read_to_string(paths::config_dir().join("config.toml")).map(|t| parse(&t)).unwrap_or_default()
}

/// config.toml 写坏了的话,坏在哪。[`load`] 不吭声(hook 不能因为它出错),命令行拿这个提醒一句
pub fn problem() -> Option<String> {
    let text = std::fs::read_to_string(paths::config_dir().join("config.toml")).ok()?;
    toml::from_str::<Config>(&text).err().map(|e| e.to_string())
}

/// `[login]` 写错了的话错在哪:那一节是单独当成没写的(见 [`lenient`]),别的照用
pub fn login_problem() -> Option<String> {
    login_problem_in(&std::fs::read_to_string(paths::config_dir().join("config.toml")).ok()?)
}

pub fn login_problem_in(text: &str) -> Option<String> {
    let login = toml::from_str::<toml::Value>(text).ok()?.get("login")?.clone();
    login.try_into::<Login>().err().map(|e| e.to_string().trim().to_string())
}

/// `[idle]` 写错了的话错在哪(这时不自动停)
pub fn idle_problem() -> Option<String> {
    idle_problem_in(&std::fs::read_to_string(paths::config_dir().join("config.toml")).ok()?)
}

pub fn idle_problem_in(text: &str) -> Option<String> {
    let idle = toml::from_str::<toml::Value>(text).ok()?.get("idle")?.clone();
    match idle.try_into::<Idle>() {
        Ok(i) => i.stop_after().err(),
        Err(e) => Some(e.to_string().trim().to_string()),
    }
}

/// 写坏了的配置当成没写:建会话、hook 都不该因为它起不来
pub fn parse(text: &str) -> Config {
    toml::from_str(text).unwrap_or_default()
}

impl Config {
    /// 工具名对应的定义;内置的几家没写也有。名字就是内置的那家、又没写 kind 的,kind 就是它;
    /// 别的名字没写 kind 的是声明式工具,kind 留空
    pub fn tool(&self, name: &str) -> Option<Tool> {
        let builtin = agent::builtin(name).is_some();
        let mut t = self.tools.get(name).cloned().or_else(|| builtin.then(Tool::default))?;
        if t.kind.is_empty() && builtin {
            t.kind = name.to_string();
        }
        Some(t)
    }

    /// 登记过的账号;没有 = None
    pub fn account(&self, kind: &str, name: &str) -> Option<&Account> {
        self.accounts.get(kind)?.get(name)
    }

    /// `n` 的菜单 / `aca new` 认的名字:工具,和「工具:账号」
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
        assert_eq!(c.names(), ["claude", "codex", "shell", "ccx", "claude:sub"]);
        // 写坏了不炸
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
        // 内置的那几家写了别的项,kind 还是它自己
        assert_eq!((c.tool("claude").unwrap().kind.as_str(), c.tool("claude").unwrap().env["B"].as_str()), ("claude", "2"));
        assert_eq!(c.names(), ["claude", "codex", "shell", "aider"]);
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
        // 不换目录的账号:home 是空的
        let api = c.account("codex", "api").unwrap();
        assert_eq!(
            (api.home.as_str(), api.env_file.as_str(), api.env["OPENAI_BASE_URL"].as_str()),
            ("", format!("{home}/k.env").as_str(), "https://x")
        );
        assert_eq!(c.names(), ["claude", "codex", "shell", "codex:api", "codex:corp", "codex:labs"]);
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
        // 字符串:Mac 上是 App 的名字,别处是命令
        let firefox = if cfg!(target_os = "macos") { vec!["open", "-a", "Firefox"] } else { vec!["Firefox"] };
        assert_eq!((b[0].name.as_str(), b[0].cmd.iter().map(String::as_str).collect::<Vec<_>>()), ("Firefox", firefox));
        assert_eq!(b[1], Browser { name: "Chrome 工作".into(), cmd: vec!["google-chrome".into(), "--profile-directory=Profile 1".into()] });
        // 不写名字:命令的文件名;~ 展开
        assert_eq!(b[2].name, "zen");
        assert!(b[2].cmd[0].ends_with("/bin/zen") && !b[2].cmd[0].starts_with('~'));
        // 简写成路径:表上也只写文件名
        let b = parse("[login]\nbrowsers = [\"/opt/x/work-chrome\", \"/Applications/Arc.app\"]\n").login.browsers;
        assert_eq!((b[0].name.as_str(), b[1].name.as_str()), ("work-chrome", "Arc"));
        assert_eq!(parse("[login]\nremote = true\n").login.remote, Remote::Yes);
        assert_eq!(parse("[login]\nremote = \"auto\"\n").login.remote, Remote::Auto);
        // 写错了:只有 [login] 当没写,账号照用;错在哪说得出来
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
        // 最后一个数没单位、别的字、单位前没数:都不认
        assert_eq!((span("3"), span("3h20"), span("3 小时"), span("h"), span("99999999999999999999d")), (None, None, None, None, None));
        // 写错了:不自动停,说得出错在哪;别的照用
        let c = parse("[accounts.claude]\nsub = \"~/.cs\"\n[idle]\nstop_after = 180\n");
        assert!(c.account("claude", "sub").is_some() && c.idle.stop_after().is_err());
        assert!(idle_problem_in("[idle]\nstop_after = 180\n").is_some_and(|e| e.contains("\"3h\"")));
        assert!(idle_problem_in("[idle]\nstop_after = \"3 hours\"\n").is_some_and(|e| e.contains("3 hours")));
        assert!(idle_problem_in("idle = 3\n").is_some());
        assert_eq!(idle_problem_in("[idle]\nstop_after = \"3h\"\n"), None);
        assert_eq!(idle_problem_in("[keys]\nnext = \"\"\n"), None);
    }
}
