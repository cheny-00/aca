//! opencode(opencode.ai)。hook 用一个插件带进去:旁边的 `opencode_plugin.js` 写进 `state/hooks/`,
//! 启动时 `OPENCODE_CONFIG_CONTENT` 里写 `{"plugin": ["file://<它>"]}`。这份配置叠在你自己的配置上面,
//! 你的插件、`OPENCODE_CONFIG_DIR` 照样加载;你自己设了 `OPENCODE_CONFIG_CONTENT` 的,aca 的插件合进去。
//! 插件把 opencode 的事件换成 hook 协议的形状报给 `aca hook opencode`。
//!
//! 1.19.3 上实测:
//! - 对话 id(`ses_…`)是它自己起的,第一句话发出去才有(分叉的一起来就有):等 hook 报
//! - `-s <id>` 接着,`-s <id> --fork` 分叉;`--prompt <话>` 起来就发
//! - 标题它自己起(拿第一句话问一次模型),插件报上来,也写在窗格标题里(`OC | <标题>`)
//! - 权限框:回车是「Allow once」,Esc 是拒绝(拒绝了这一轮就停下,不报错)
//! - 打断要连按两下 Esc(第一下只提示「esc again to interrupt」),两下要分开按
//! - 上游 5xx 它一直退避重试(屏幕上写着第几次),还算在跑;4xx 直接报错
//! - task 工具派出去的子 agent 也是一段会话,插件认 `parentID` 滤掉
//! - 退出时插件收不到任何事件:列表看窗格没了认出退出
//! - 登录在 `$XDG_DATA_HOME/opencode/auth.json`,对话在同一个目录的 `opencode.db`:没有一个变量只换一个账号的目录,
//!   账号里只能写 env / args(各家的 API key、`OPENCODE_AUTH_CONTENT`)

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use super::{Agent, How, Live};
use crate::config::Tool;
use crate::paths::Res;
use crate::runtime::Run;
use crate::screen::Seen;
use crate::store::Session;

pub struct Opencode;

const PLUGIN: &str = include_str!("opencode_plugin.js");
const CONTENT: &str = "OPENCODE_CONFIG_CONTENT";

/// 在跑时底栏写着「esc interrupt」,按过一下 Esc 后是「esc again to interrupt」
static RUNNING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\besc (?:again to )?interrupt\b").unwrap());

fn plan(tool: &Tool, s: &Session, how: &How) -> Vec<String> {
    let mut argv = super::base(tool, "opencode");
    match how {
        How::Fresh => {}
        How::Resume(conv) => argv.extend(["-s".to_string(), conv.clone()]),
        How::Fork(from) => argv.extend(["-s".to_string(), from.clone(), "--fork".to_string()]),
    }
    argv.extend(super::extra(tool, s));
    argv
}

/// 叠上去的那份配置:你写的(有的话)加上 aca 的插件,和开着的 aca 插件给的 MCP 服务(同名的以 aca 的为准)
fn content(theirs: Option<&str>, plugin: &str, mcp: serde_json::Map<String, Value>) -> String {
    let mut v: Value = theirs.and_then(|t| serde_json::from_str(t).ok()).filter(Value::is_object).unwrap_or_else(|| json!({}));
    let top = v.as_object_mut().unwrap();
    let list = top.entry("plugin").or_insert_with(|| json!([]));
    match list.as_array_mut() {
        Some(a) => a.push(json!(plugin)),
        None => *list = json!([plugin]),
    }
    if !mcp.is_empty() {
        let servers = top.entry("mcp").or_insert_with(|| json!({}));
        match servers.as_object_mut() {
            Some(s) => s.extend(mcp),
            None => *servers = Value::Object(mcp),
        }
    }
    v.to_string()
}

/// 你自己的那份:工具、账号里写的(`set`),没写就是外面环境里的
fn theirs(set: &[(String, String)], outside: Option<String>) -> Option<String> {
    let mine = set.iter().rev().find(|(k, _)| k == CONTENT).map(|(_, v)| v.clone());
    mine.or(outside).filter(|v| !v.trim().is_empty())
}

/// 接管时不照搬的:接哪段对话的那几个开关
fn not_carried(rest: &[String]) -> usize {
    match rest[0].as_str() {
        "-c" | "--continue" | "--fork" => 1,
        _ => super::valued(rest, &["-s", "--session"]),
    }
}

/// 窗格标题 `OC | 修登录` → `修登录`;还没起标题时是 `OpenCode`
fn pane_title(title: &str) -> Option<String> {
    let name = title.trim().strip_prefix("OC | ")?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// 框线、左边那条竖线去掉
fn bare(l: &str) -> &str {
    l.trim().trim_start_matches(['┃', '╹', '│']).trim()
}

impl Agent for Opencode {
    fn name(&self) -> &str {
        "opencode"
    }

    fn can_fork(&self) -> bool {
        true
    }

    fn argv(&self, tool: &Tool, s: &Session, how: &How, _aca: &str) -> Res<Vec<String>> {
        Ok(plan(tool, s, how))
    }

    fn env(&self, tool: &Tool, aca: &str, set: &[(String, String)]) -> Res<Vec<(String, String)>> {
        let file = super::hook_file("opencode.js", PLUGIN)?;
        let outside = std::env::var(CONTENT).ok();
        let mcp = crate::plugin::mount::opencode_mcp(aca, &tool.mcp);
        Ok(vec![(CONTENT.to_string(), content(theirs(set, outside).as_deref(), &format!("file://{}", file.display()), mcp))])
    }

    /// 知道对话 id 就接(第一句话发出去才有);分叉出来没说过话的从原来那段重新分
    fn resume(&self, conv: &str, r: &Run) -> How {
        match (conv.is_empty(), r.fork_from.is_empty()) {
            (false, _) => How::Resume(conv.to_string()),
            (true, false) => How::Fork(r.fork_from.clone()),
            (true, true) => How::Fresh,
        }
    }

    /// 插件报的标题;没报(接管来还没重启的)看窗格标题
    fn title(&self, r: &Run, pane: &str) -> Option<String> {
        (!r.hook_title.is_empty()).then(|| r.hook_title.clone()).or_else(|| pane_title(pane))
    }

    fn live(&self, _pid: u32, _home: Option<&str>, argv: &[String]) -> Live {
        let at = argv.iter().position(|a| a == "-s" || a == "--session");
        let conv = at.and_then(|i| argv.get(i + 1)).filter(|v| !v.starts_with('-')).cloned().unwrap_or_default();
        Live { conv, args: super::carry(argv, not_carried), ..Default::default() }
    }

    /// 「Allow once   Allow always   Reject」:光标一开始在 Allow once 上,回车同意;Esc 拒绝
    fn permission_keys(&self) -> Option<(&'static str, &'static str)> {
        Some(("Enter", "Escape"))
    }

    fn interrupt_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["Escape", "Escape"])
    }

    fn reports_interrupt(&self) -> bool {
        true
    }

    /// `--prompt <那句话>`:起来就发。横线开头的会被当成开关,那样的起来之后打字进去
    fn opening(&self, text: &str) -> Option<Vec<String>> {
        (!text.starts_with('-')).then(|| vec!["--prompt".to_string()])
    }

    /// 权限框(问的那句取框里的命令),底栏的「esc interrupt」。别的(问你的题长什么样不一定)认不出,听 hook 的
    fn screen(&self, lines: &[String]) -> Seen {
        let rows: Vec<&str> = lines.iter().map(|l| bare(l)).filter(|l| !l.is_empty()).collect();
        let tail = &rows[rows.len().saturating_sub(12)..];
        if let Some(i) = tail.iter().position(|l| l.ends_with("Permission required")) {
            let what = tail[i + 1..].iter().find(|l| l.starts_with("$ ")).or(tail.get(i + 1)).copied().unwrap_or("");
            return Seen::Ask(what.trim_start_matches(['#', '$', ' ']).to_string());
        }
        if tail.iter().rev().take(2).any(|l| RUNNING.is_match(l)) { Seen::Running(0) } else { Seen::Unsure }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn it_resumes_and_forks_with_the_session_flag() {
        let t = Tool { args: vec!["--agent".into(), "plan".into()], ..Default::default() };
        let s = Session { args: vec!["-m".into(), "fake/fake-1".into()], ..Default::default() };
        let run = |how: How| plan(&t, &s, &how).join(" ");
        assert_eq!(run(How::Fresh), "opencode --agent plan -m fake/fake-1");
        assert_eq!(run(How::Resume("ses_1".into())), "opencode -s ses_1 --agent plan -m fake/fake-1");
        assert_eq!(run(How::Fork("ses_1".into())), "opencode -s ses_1 --fork --agent plan -m fake/fake-1");
        assert_eq!(Opencode.resume("ses_2", &Run::default()), How::Resume("ses_2".into()));
        assert_eq!(Opencode.resume("", &Run { fork_from: "ses_1".into(), ..Default::default() }), How::Fork("ses_1".into()));
        assert_eq!(Opencode.resume("", &Run::default()), How::Fresh);
    }

    #[test]
    fn the_plugin_rides_on_top_of_whatever_config_content_you_set() {
        let v = |s: String| serde_json::from_str::<Value>(&s).unwrap();
        let none = serde_json::Map::new;
        assert_eq!(v(content(None, "file:///st/hooks/opencode.js", none())), json!({ "plugin": ["file:///st/hooks/opencode.js"] }));
        let yours = r#"{"model":"x/y","plugin":["opencode-wakatime"]}"#;
        assert_eq!(
            v(content(Some(yours), "file:///p.js", none())),
            json!({ "model": "x/y", "plugin": ["opencode-wakatime", "file:///p.js"] })
        );
        // 写坏了的、不是对象的:只放 aca 的
        assert_eq!(v(content(Some("not json"), "file:///p.js", none())), json!({ "plugin": ["file:///p.js"] }));
        assert_eq!(v(content(Some(r#"{"plugin":"one"}"#), "file:///p.js", none())), json!({ "plugin": ["file:///p.js"] }));
        // 插件的 MCP 服务:和你的放在一起
        let mcp = crate::plugin::mount::opencode_mcp("/bin/aca", &[("aca".into(), vec!["plugin".into(), "tools".into()])]);
        let yours = r#"{"mcp":{"gh":{"type":"remote","url":"https://x"}}}"#;
        assert_eq!(
            v(content(Some(yours), "file:///p.js", mcp))["mcp"],
            json!({ "gh": { "type": "remote", "url": "https://x" }, "aca": { "type": "local", "command": ["/bin/aca", "plugin", "tools"], "enabled": true } })
        );
    }

    #[test]
    fn your_config_content_comes_from_the_account_layer_before_the_environment() {
        let set = vec![("X".to_string(), "1".to_string()), (CONTENT.to_string(), r#"{"share":"disabled"}"#.to_string())];
        assert_eq!(theirs(&set, Some("{}".into())).as_deref(), Some(r#"{"share":"disabled"}"#));
        assert_eq!(theirs(&[], Some(r#"{"a":1}"#.into())).as_deref(), Some(r#"{"a":1}"#));
        assert_eq!(theirs(&[], Some(" ".into())), None);
    }

    #[test]
    fn the_screen_shows_the_permission_box_and_the_interrupt_hint() {
        // 1.19.3 上抓的
        let ask = "  ┃  RUN:echo hi-from-bash\n  ┃\n     $ echo hi-from-bash\n     ▣  Build · Fake One\n  ┃\n  ┃  △ Permission required\n  ┃    # Shell command\n  ┃\n  ┃  $ echo hi-from-bash\n  ┃\n  ┃\n  ┃   Allow once   Allow always   Reject                                 ctrl+f fullscreen  ⇆ select  enter confirm\n  ┃\n";
        assert_eq!(Opencode.screen(&lines(ask)), Seen::Ask("echo hi-from-bash".into()));
        let busy = "  ┃  Build · Fake One Fake\n  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀\n   ■■■■■■⬝⬝  esc interrupt                       tab agents  ctrl+p commands\n";
        assert_eq!(Opencode.screen(&lines(busy)), Seen::Running(0));
        let again = "  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀\n   ⬝⬝⬝⬝⬝⬝⬝⬝  esc again to interrupt                15 (0%)  ctrl+p commands\n";
        assert_eq!(Opencode.screen(&lines(again)), Seen::Running(0));
        let retry = "   ⬝⬝⬝⬝⬝⬝⬝⬝ fake upstream exploded [retrying in 3s attempt #4]                esc interrupt\n";
        assert_eq!(Opencode.screen(&lines(retry)), Seen::Running(0));
        let idle = "     ok: hello there friend\n     ▣  Build · Fake One · 2.6s\n  ┃  Build · Fake One Fake\n  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀\n   /home/dev/proj                                    15 (0%)  ctrl+p commands\n";
        assert_eq!(Opencode.screen(&lines(idle)), Seen::Unsure);
    }

    #[test]
    fn titles_come_from_the_plugin_or_the_pane() {
        assert_eq!(pane_title("OC | Fake title hello there").as_deref(), Some("Fake title hello there"));
        assert_eq!(pane_title("OpenCode"), None);
        let said = Run { hook_title: "它报的".into(), ..Default::default() };
        assert_eq!(Opencode.title(&said, "OC | x").as_deref(), Some("它报的"));
        assert_eq!(Opencode.title(&Run::default(), "OC | x").as_deref(), Some("x"));
    }

    #[test]
    fn the_opening_rides_on_prompt_unless_it_looks_like_a_flag() {
        assert_eq!(Opencode.opening("修一下登录"), Some(vec!["--prompt".to_string()]));
        assert_eq!(Opencode.opening("-v 是什么"), None);
    }

    #[test]
    fn adopting_takes_the_session_flag_and_carries_the_rest() {
        let argv: Vec<String> = ["opencode", "-s", "ses_9", "-m", "fake/fake-1"].map(String::from).to_vec();
        let l = Opencode.live(1, None, &argv);
        assert_eq!((l.conv.as_str(), l.args.unwrap()), ("ses_9", vec!["-m".to_string(), "fake/fake-1".to_string()]));
    }
}
