//! pi(earendil-works/pi)。hook 用 `-e <扩展>` 在启动时带进去:扩展是旁边的 `pi_ext.ts`,写进 `state/hooks/`,
//! 它把 pi 的事件换成 hook 协议的形状报给 `aca hook pi`。对话 id 能在启动参数里指定(`--session-id`),和 Claude 一样登记时就定下来。
//!
//! 1.3.0 上实测:
//! - `--session-id <id>`:有这段就接着,没有就用这个 id 新开。只在当前目录的那个项目里找,换了目录会用同一个 id 新开一段
//! - `--fork <id> --session-id <新 id>`:分叉成新的一段(名字也带过去)
//! - 上游出错它自己重试(3 次),每试一次都有一个 `agent_end`,`agent_settled` 才是真的结束;Esc 打断它报 `aborted`
//! - 没有自带的权限框;装了权限类扩展的,一轮里弹的框报成「在等你」
//! - 不自己起标题。`/name` 起的名字它报上来,也写在窗格标题里(`π - <名字> - <目录>`)
//! - 进程名就是 `pi`

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use super::{Agent, How, Live, tail};
use crate::config::Tool;
use crate::paths::{self, Res};
use crate::runtime::Run;
use crate::screen::Seen;
use crate::store::Session;

pub struct Pi;

const EXTENSION: &str = include_str!("pi_ext.ts");

/// 在跑时输入框上面那条分隔线:「── ⠹ Working ─────」
static WORKING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^─+ \S+ Working\b").unwrap());

fn plan(tool: &Tool, s: &Session, how: &How, ext: &str) -> Vec<String> {
    let mut argv = super::base(tool, "pi");
    let mut a = |xs: &[&str]| argv.extend(xs.iter().map(|x| x.to_string()));
    match how {
        How::Fresh if s.conv.is_empty() => {}
        How::Fresh => a(&["--session-id", &s.conv]),
        How::Resume(conv) => a(&["--session-id", conv]),
        How::Fork(from) => {
            a(&["--fork", from]);
            if !s.conv.is_empty() {
                a(&["--session-id", &s.conv]);
            }
        }
    }
    a(&["-e", ext]);
    argv.extend(super::extra(tool, s));
    argv
}

/// 接管时不照搬的:接哪段对话的那几个开关、aca 带进去的扩展(起的时候会重新拼)
fn not_carried(rest: &[String]) -> usize {
    match rest[0].as_str() {
        "-c" | "--continue" | "-r" | "--resume" => 1,
        "-e" | "--extension" if rest.get(1).is_some_and(|v| v.ends_with("/hooks/pi.ts")) => 2,
        _ => super::valued(rest, &["--session-id", "--session", "--fork"]),
    }
}

/// 窗格标题 `π - 修登录 - api` → `修登录`;没起名字时只有 `π - api`
fn pane_title(title: &str) -> Option<String> {
    let rest = title.trim().strip_prefix("π - ")?;
    let (name, _dir) = rest.rsplit_once(" - ")?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// 对话记录里最后一段回复的字:被打断的、出错的那几条不算
fn last_reply(text: &str) -> Option<String> {
    text.lines().rev().filter(|l| l.contains("\"assistant\"")).find_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        let m = &v["message"];
        if v["type"] != "message" || m["role"] != "assistant" || matches!(m["stopReason"].as_str(), Some("error" | "aborted")) {
            return None;
        }
        let parts = m["content"].as_array()?;
        let reply: String = parts.iter().filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect();
        (!reply.trim().is_empty()).then_some(reply)
    })
}

impl Agent for Pi {
    fn name(&self) -> &str {
        "pi"
    }

    fn home_var(&self) -> Option<&str> {
        Some("PI_CODING_AGENT_DIR")
    }

    fn default_home(&self) -> Option<&str> {
        Some("~/.pi/agent")
    }

    /// 对话记录 `sessions/--<目录>--/<时刻>_<id>.jsonl`
    fn conv_files(&self, home: &Path, conv: &str) -> Vec<PathBuf> {
        if conv.is_empty() || conv.contains('/') {
            return vec![];
        }
        let tail = format!("_{conv}.jsonl");
        let Ok(dirs) = std::fs::read_dir(home.join("sessions")) else { return vec![] };
        let found = dirs.flatten().filter(|d| d.path().is_dir()).find_map(|d| {
            let f = std::fs::read_dir(d.path()).ok()?.flatten().find(|f| f.file_name().to_string_lossy().ends_with(&tail))?;
            Some(Path::new("sessions").join(d.file_name()).join(f.file_name()))
        });
        found.into_iter().collect()
    }

    fn new_conv(&self) -> String {
        paths::uuid4()
    }

    fn can_fork(&self) -> bool {
        true
    }

    fn argv(&self, tool: &Tool, s: &Session, how: &How, _aca: &str) -> Res<Vec<String>> {
        let ext = super::hook_file("pi.ts", EXTENSION)?;
        Ok(plan(tool, s, how, &ext.to_string_lossy()))
    }

    /// 开着的插件给的 MCP 服务:一份按内容起名的清单,路径放在 `ACA_PI_MCP` 里,aca 的扩展起来时一个个挂上(只对这一次启动)
    fn env(&self, tool: &Tool, aca: &str, _set: &[(String, String)]) -> Res<Vec<(String, String)>> {
        if tool.mcp.is_empty() {
            return Ok(vec![]);
        }
        let text = crate::plugin::mount::pi_mcp_config(aca, &tool.mcp);
        let mut h = DefaultHasher::new();
        text.hash(&mut h);
        let file = super::hook_file(&format!("pi-mcp-{:016x}.json", h.finish()), &text)?;
        Ok(vec![("ACA_PI_MCP".to_string(), file.to_string_lossy().into_owned())])
    }

    /// 对话记录在就接着;分叉出来没说过话的从原来那段重新分;别的用登记的 id 新开(`--session-id` 有就接、没有就新开)
    fn resume(&self, conv: &str, r: &Run) -> How {
        let known = !conv.is_empty() && Path::new(&r.transcript).is_file();
        match (known, r.fork_from.is_empty()) {
            (true, _) => How::Resume(conv.to_string()),
            (false, false) => How::Fork(r.fork_from.clone()),
            (false, true) => How::Fresh,
        }
    }

    /// hook 报的名字;没报(接管来还没重启的)看窗格标题
    fn title(&self, r: &Run, pane: &str) -> Option<String> {
        (!r.hook_title.is_empty()).then(|| r.hook_title.clone()).or_else(|| pane_title(pane))
    }

    fn live(&self, _pid: u32, _home: Option<&str>, argv: &[String]) -> Live {
        let at = argv.iter().position(|a| a == "--session-id");
        let conv = at.and_then(|i| argv.get(i + 1)).cloned().unwrap_or_default();
        Live { conv, args: super::carry(argv, not_carried), ..Default::default() }
    }

    fn last_reply(&self, r: &Run) -> Option<String> {
        last_reply(&tail(&r.transcript, 1024 * 1024)?)
    }

    /// Esc 一下就停,扩展报 `Interrupt`
    fn interrupt_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["Escape"])
    }

    fn reports_interrupt(&self) -> bool {
        true
    }

    /// `pi -- <那句话>`:起来就发
    fn opening(&self, text: &str) -> Option<Vec<String>> {
        super::opening_after_dashes(text)
    }

    /// 在跑时分隔线上写着 Working。别的(扩展弹的框长什么样不一定)认不出,听 hook 的
    fn screen(&self, lines: &[String]) -> Seen {
        let nonempty: Vec<&str> = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
        let tail = &nonempty[nonempty.len().saturating_sub(8)..];
        if tail.iter().any(|l| WORKING.is_match(l)) { Seen::Running(0) } else { Seen::Unsure }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    fn sess(conv: &str) -> Session {
        Session { conv: conv.into(), args: vec!["--model".into(), "fake-1".into()], ..Default::default() }
    }

    #[test]
    fn it_names_the_conversation_up_front_and_forks_into_a_new_one() {
        let t = Tool { args: vec!["--thinking".into(), "high".into()], ..Default::default() };
        let run = |s: &Session, how: How| plan(&t, s, &how, "/st/hooks/pi.ts").join(" ");
        assert_eq!(run(&sess("c-1"), How::Fresh), "pi --session-id c-1 -e /st/hooks/pi.ts --thinking high --model fake-1");
        assert_eq!(run(&sess(""), How::Fresh), "pi -e /st/hooks/pi.ts --thinking high --model fake-1");
        assert_eq!(run(&sess("c-1"), How::Resume("c-9".into())), "pi --session-id c-9 -e /st/hooks/pi.ts --thinking high --model fake-1");
        assert_eq!(
            run(&sess("c-2"), How::Fork("c-1".into())),
            "pi --fork c-1 --session-id c-2 -e /st/hooks/pi.ts --thinking high --model fake-1"
        );
        // 自己定了命令:接在它后面
        let wrapped = Tool { cmd: vec!["/w/pi.sh".into()], ..Default::default() };
        assert_eq!(plan(&wrapped, &sess("c"), &How::Fresh, "/e")[..3], ["/w/pi.sh", "--session-id", "c"]);
    }

    #[test]
    fn it_resumes_when_the_transcript_is_there_and_refork_when_a_fork_never_spoke() {
        let d = std::env::temp_dir().join(format!("aca-pi-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let t = d.join("2026-10-20T03-41-50-405Z_c1.jsonl");
        std::fs::write(&t, "{}\n").unwrap();
        let r = Run { transcript: t.to_string_lossy().into(), ..Default::default() };
        assert_eq!(Pi.resume("c1", &r), How::Resume("c1".into()));
        assert_eq!(Pi.resume("c1", &Run::default()), How::Fresh);
        assert_eq!(Pi.resume("", &Run { fork_from: "c0".into(), ..Default::default() }), How::Fork("c0".into()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_conversation_files_are_found_by_id_under_the_project_directory() {
        let home = std::env::temp_dir().join(format!("aca-pi-home-{}", std::process::id()));
        let dir = home.join("sessions").join("--home-dev-proj--");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("2026-10-20T03-41-50-405Z_aca-test-1.jsonl"), "{}\n").unwrap();
        std::fs::write(dir.join("2026-10-20T03-43-26-465Z_other.jsonl"), "{}\n").unwrap();
        assert_eq!(Pi.conv_files(&home, "aca-test-1"), [Path::new("sessions/--home-dev-proj--/2026-10-20T03-41-50-405Z_aca-test-1.jsonl")]);
        assert!(Pi.conv_files(&home, "nope").is_empty() && Pi.conv_files(&home, "../x").is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_last_reply_skips_aborted_and_failed_turns() {
        // 1.3.0 的记录里抓的几行(跑完一轮、被 Esc 打断、上游 400)
        let log = r#"{"type":"message","id":"7831cf92","parentId":"1fc152b8","message":{"role":"assistant","content":[{"type":"text","text":"the command ran: x\n"}],"stopReason":"stop","errorMessage":null}}
{"type":"message","id":"8ddfee5a","parentId":"a44f3d2f","message":{"role":"assistant","content":[{"type":"text","text":"slow slow slow "}],"stopReason":"aborted","errorMessage":"Operation aborted"}}
{"type":"message","id":"ebf2e912","parentId":"7ffb91a6","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"400: {\"message\":\"fake model rejected the request\"}"}}"#;
        assert_eq!(last_reply(log).as_deref(), Some("the command ran: x\n"));
        assert_eq!(last_reply(r#"{"type":"message","message":{"role":"user","content":"hi"}}"#), None);
    }

    #[test]
    fn the_screen_says_running_only_while_the_working_line_is_up() {
        let busy = " ok: hello there friend\n── ⠹ Working ─────────────────────\n─────────────────────────────────\n~/proj (master)\n1.4%/100k (auto)        fake-1\n";
        assert_eq!(Pi.screen(&lines(busy)), Seen::Running(0));
        let idle = " ok: hello there friend\n─────────────────────────────────\n─────────────────────────────────\n~/proj (master)\n↑10 ↓5 0.0%/100k (auto)   fake-1\n";
        assert_eq!(Pi.screen(&lines(idle)), Seen::Unsure);
        // 正文里提到 Working 的不算
        assert_eq!(Pi.screen(&lines("I am Working on it\n────\n~/proj\n")), Seen::Unsure);
    }

    #[test]
    fn the_title_is_the_reported_name_or_else_the_middle_of_the_pane_title() {
        assert_eq!(pane_title("π - fix the login - proj").as_deref(), Some("fix the login"));
        assert_eq!(pane_title("π - a - b - proj").as_deref(), Some("a - b"));
        assert_eq!(pane_title("π - proj"), None);
        assert_eq!(pane_title("my-host"), None);
        let named = Run { hook_title: "它报的".into(), ..Default::default() };
        assert_eq!(Pi.title(&named, "π - x - proj").as_deref(), Some("它报的"));
        assert_eq!(Pi.title(&Run::default(), "π - x - proj").as_deref(), Some("x"));
    }

    #[test]
    fn adopting_takes_the_conversation_and_carries_the_other_flags() {
        let argv: Vec<String> =
            ["pi", "-e", "/st/hooks/pi.ts", "--session-id", "c-7", "--model", "fake-1", "-e", "./mine.ts"].map(String::from).to_vec();
        let l = Pi.live(1, None, &argv);
        assert_eq!(l.conv, "c-7");
        assert_eq!(l.args.unwrap(), ["--model", "fake-1", "-e", "./mine.ts"]);
        assert_eq!(Pi.live(1, None, &["pi".to_string(), "-c".to_string()]).args.unwrap(), Vec::<String>::new());
    }
}
