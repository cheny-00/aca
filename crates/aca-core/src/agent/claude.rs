//! Claude Code。hook 用 `--settings <文件>` 在启动时带进去;对话 id 自己定(`--session-id`);
//! 标题读它写在窗格标题里的那个(没有才读对话记录里的);屏幕上认选择框和「在跑」那一行。

use std::hash::{DefaultHasher, Hash, Hasher};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use super::{Agent, How, Live};
use crate::config::Tool;
use crate::paths::{self, Res};
use crate::runtime::Run;
use crate::screen::{self, Seen};
use crate::store::Session;
use crate::{t, tr};

pub struct Claude;

/// 只挂状态要的这几个事件。工具事件只挂 AskUserQuestion(它在问你),不挂 `*`:那样每次工具调用都要多起一个进程
const EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    ("Stop", None),
    ("StopFailure", None),
    ("Notification", None),
    ("PermissionRequest", None),
    ("PreCompact", None),
    ("PostCompact", None),
    ("SessionEnd", None),
    ("PreToolUse", Some("AskUserQuestion")),
    ("PostToolUse", Some("AskUserQuestion")),
];

static ASK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)Enter to select|esc to cancel|Do you want to|Would you like to").unwrap());
static RUNNING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\S\s+\S+…\s+\((?:(\d+)h\s*)?(?:(\d+)m\s*)?(?:(\d+)s)?").unwrap());
/// 一轮刚开始、第一个字还没回来时(模型慢的十几秒),那一行只有「· Combobulating…」,后面没有计时。
/// 这样的只在输入框上面紧挨着的几行里认,开头得是它转的那几个字符:正文里也会有省略号结尾的行
static STARTING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[·✢✳✶✻✽]\s+\S.*?…(?:\s+\((?:(\d+)h\s*)?(?:(\d+)m\s*)?(?:(\d+)s)?.*)?\s*$").unwrap());
/// 输入框上面看几行:「在跑」那一行和输入框之间只夹着一两行提示(`⎿  Tip: …`)
const NEAR: usize = 6;
/// 正文的尾巴:输入框上面这么多行(见 `body`)
const BODY: usize = 8;

/// 输入框在第几行:屏幕上最下面那个 `❯` 开头的行,头上、脚下各有一条横线。上面那些 `❯` 是你说过的话;
/// 确认框 / 选择框弹出来时输入框会收起来,里面的 `❯` 是选项前的光标,不会两头都是横线
fn input_at(tail: &[&str]) -> Option<usize> {
    let rule = |l: &&str| l.trim_start().starts_with('─');
    let at = tail.iter().rposition(|l| l.trim_start().starts_with('❯'))?;
    (at > 0 && rule(&tail[at - 1]) && tail[at + 1..].iter().any(rule)).then_some(at)
}

/// 那一行里的计时(1h 9m 1s)→ 秒;没有计时 = 0
fn secs(c: &regex::Captures) -> u64 {
    let n = |i: usize| c.get(i).and_then(|m| m.as_str().parse::<u64>().ok()).unwrap_or(0);
    n(1) * 3600 + n(2) * 60 + n(3)
}

/// `--settings` 的那份文件的内容。除了状态要的那几个事件,还有门铃(会话之间递话,见 [`crate::inbox`]):
/// - SessionStart 多跑一条 `aca inbox watch`:回一个 `watchPaths`,让 Claude 自己盯住这个会话的门铃文件
/// - 门铃一变(FileChanged)就在后台跑 `aca inbox take`(`asyncRewake`):有没送的消息就写到 stderr、退出码 2,
///   Claude 把它当成系统提示给模型 —— 空着就开一轮,在跑就塞进这一轮,在等你就等你答完再送;没有就退出 0,什么都不发生。
///   `rewakeMessage` 是那段前面的记号,`rewakeSummary` 是界面上那一行(2.1.294 上实测)
fn settings(aca: &str) -> String {
    let q = super::sh_quote(aca);
    let command = format!("{q} hook claude");
    let mut hooks: serde_json::Map<String, Value> = EVENTS
        .iter()
        .map(|(ev, matcher)| {
            let mut entry = json!({ "hooks": [{ "type": "command", "command": command, "timeout": 10 }] });
            if let Some(m) = matcher {
                entry["matcher"] = json!(m);
            }
            (ev.to_string(), json!([entry]))
        })
        .collect();
    if let Some(list) = hooks["SessionStart"][0]["hooks"].as_array_mut() {
        list.push(json!({ "type": "command", "command": format!("{q} inbox watch"), "timeout": 10 }));
    }
    // 起来、每发一句时把会话的名字交给它当它自己的名字(见 hook::name_cmd)
    for ev in ["SessionStart", "UserPromptSubmit"] {
        if let Some(list) = hooks[ev][0]["hooks"].as_array_mut() {
            list.push(json!({ "type": "command", "command": format!("{q} hook-name"), "timeout": 10 }));
        }
    }
    let take = json!({
        "type": "command", "command": format!("{q} inbox take"), "timeout": 30,
        "asyncRewake": true, "rewakeMessage": "[aca]", "rewakeSummary": "aca: 收件箱有新消息",
    });
    hooks.insert("FileChanged".into(), json!([{ "hooks": [take] }]));
    serde_json::to_string_pretty(&json!({ "hooks": hooks })).unwrap()
}

/// 你自己带的 `--settings`(工具、账号、会话的参数里)拿出来。Claude 只认最后一个 `--settings`(2.1.288 上实测:
/// 两个并排带,前一个里的 hook 一个也不跑),所以不能和 aca 那份并排,得合成一份([`merge`])
fn take_settings(extra: &mut Vec<String>) -> Option<String> {
    let (mut found, mut i) = (None, 0);
    while i < extra.len() {
        if extra[i] == "--settings" && i + 1 < extra.len() {
            found = Some(extra.remove(i + 1));
            extra.remove(i);
        } else if let Some(v) = extra[i].strip_prefix("--settings=") {
            found = Some(v.to_string());
            extra.remove(i);
        } else {
            i += 1;
        }
    }
    found
}

/// aca 的 hook 并进你那份 settings(一个文件,或者直接一段 JSON):同一个事件下你的在前、aca 的接在后面,别的原样。
/// 相对路径按会话的目录算(Claude 自己也是在那读)
fn merge(ours: &str, theirs: &str, dir: &str) -> Res<String> {
    let text = if theirs.trim_start().starts_with('{') {
        theirs.to_string()
    } else {
        let file = Path::new(dir).join(paths::expand(theirs));
        std::fs::read_to_string(&file)
            .map_err(|e| tr!("你带的 --settings 读不了 {}:{e}", "can't read the --settings you passed {}: {e}", file.display()))?
    };
    let mut all: Value =
        serde_json::from_str(&text).map_err(|e| tr!("你带的 --settings 不是 JSON:{e}", "the --settings you passed isn't JSON: {e}"))?;
    let ours: Value = serde_json::from_str(ours).unwrap();
    let hooks = all
        .as_object_mut()
        .ok_or(t!("你带的 --settings 不是一个 JSON 对象", "the --settings you passed isn't a JSON object"))?
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let hooks =
        hooks.as_object_mut().ok_or(t!("你带的 --settings 里 hooks 不是对象", "hooks in the --settings you passed isn't an object"))?;
    for (ev, entries) in ours["hooks"].as_object().unwrap() {
        let mine = hooks.entry(ev.clone()).or_insert_with(|| json!([]));
        let mine = mine
            .as_array_mut()
            .ok_or_else(|| tr!("你带的 --settings 里 hooks.{ev} 不是数组", "hooks.{ev} in the --settings you passed isn't an array"))?;
        mine.extend(entries.as_array().unwrap().iter().cloned());
    }
    Ok(serde_json::to_string_pretty(&all).unwrap())
}

/// 纯的那一半:不写文件。`settings` 是要带的那一份(你自己带了的话,已经是合好的),`mods` 是 aca 的 mod 的目录(不带 = None),
/// `mcp` 是插件的 MCP 服务那份 `--mcp-config`(没有 = None)
fn plan(tool: &Tool, s: &Session, how: &How, settings: &str, mods: Option<&str>, mcp: Option<&str>) -> Vec<String> {
    let mut argv = super::base(tool, "claude");
    let mut a = |xs: &[&str]| argv.extend(xs.iter().map(|x| x.to_string()));
    match how {
        How::Fresh if s.conv.is_empty() => {}
        How::Fresh => a(&["--session-id", &s.conv]),
        How::Resume(conv) => a(&["--resume", conv]),
        How::Fork(from) => {
            a(&["--resume", from, "--fork-session"]);
            if !s.conv.is_empty() {
                a(&["--session-id", &s.conv]);
            }
        }
    }
    // --mcp-config 后面能跟好几个文件:紧跟一个开关,免得把后面你的参数也当成它的
    if let Some(f) = mcp {
        a(&["--mcp-config", f]);
    }
    a(&["--settings", settings]);
    if let Some(dir) = mods {
        a(&["--plugin-dir", dir]);
    }
    // 开着的 aca 插件带来的 Claude 插件(命令、技能……):只对这一次启动有效,不装进它的配置目录。能重复带
    for d in &tool.plugin_dirs {
        a(&["--plugin-dir", d]);
    }
    let mut extra = super::extra(tool, s);
    take_settings(&mut extra);
    argv.extend(extra);
    argv
}

/// 窗格标题开头它写的记号:闲着是 ✳,干活时转 ◐ ◑(老版本转的是盲文点);在 tmux 里它不转,一直是 ✳
const MARKS: &str = "✳◐◑";
/// 还没起标题时它写的
const UNTITLED: &str = "Claude Code";

/// 窗格标题 `✳ 修登录` → `修登录`:你 /rename 的,没有就是它自己起的,和它自己界面上显示的是同一个。
/// 开头不是它的记号的(关了终端标题:tmux 默认的主机名、shell 写的)、还没起标题的不算
fn shown_title(pane_title: &str) -> Option<String> {
    let mut cs = pane_title.chars();
    let mark = cs.next()?;
    if !(MARKS.contains(mark) || screen::spinning(pane_title)) || cs.next() != Some(' ') {
        return None;
    }
    let t = cs.as_str().trim();
    (!t.is_empty() && t != UNTITLED).then(|| t.to_string())
}

/// 对话记录里的标题:你改的名(custom-title),没有就是它自己起的(ai-title),各看最后写下的那条。只读文件末尾一段。
/// 不是「哪条写得晚用哪条」:它隔一阵把这两条都重写一遍,自己起的那条写在后面
/// 对话记录结尾最后一段带字的回复。分出去的子 agent 说的、Claude Code 自己垫的(model 是 `<synthetic>`)不算
fn transcript_reply(transcript: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    // 回复后面可能还跟着几大段工具的输出:比读标题多往回看一些
    const TAIL: u64 = 1024 * 1024;
    let mut f = std::fs::File::open(transcript).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines().rev().filter(|l| l.contains("\"type\":\"assistant\"")).find_map(|l| {
        let v: Value = serde_json::from_str(l).ok()?;
        if v["type"] != "assistant" || v["isSidechain"] == true || v["message"]["model"] == "<synthetic>" {
            return None;
        }
        let parts: Vec<&str> =
            v["message"]["content"].as_array()?.iter().filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect();
        let reply = parts.join("\n");
        (!reply.trim().is_empty()).then_some(reply)
    })
}

fn transcript_title(transcript: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 256 * 1024;
    let mut f = std::fs::File::open(transcript).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let last = |kind: &str, key: &str| {
        let tag = format!("\"type\":\"{kind}\"");
        text.lines().rev().find_map(|l| {
            if !l.contains(&tag) {
                return None;
            }
            let v: Value = serde_json::from_str(l).ok()?;
            let t = v.get(key)?.as_str()?.trim();
            (!t.is_empty()).then(|| t.to_string())
        })
    };
    last("custom-title", "customTitle").or_else(|| last("ai-title", "aiTitle"))
}

/// Claude 把项目目录变成 projects 底下的文件夹名:不是字母数字的都换成 -
fn project_key(dir: &str) -> String {
    dir.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// 命令行里「接哪段对话」的那几个,和 aca 自己带进去的那份 hook、那个 mod、aca 插件的目录:接管时不照搬(起的时候会重新拼)
fn not_carried(rest: &[String]) -> usize {
    if matches!(rest[0].as_str(), "--continue" | "-c" | "--fork-session") {
        return 1;
    }
    let n = super::valued(rest, &["--session-id"]).max(super::optional(rest, &["--resume", "-r"]));
    if n > 0 {
        return n;
    }
    let n = super::valued(rest, &["--settings", "--plugin-dir", "--mcp-config"]);
    let file = if n == 2 { rest.get(1).map(String::as_str) } else { rest[0].split_once('=').map(|(_, v)| v) };
    if rest[0].starts_with("--plugin-dir") && file.is_some_and(crate::plugin::is_agent_dir) {
        return n;
    }
    if file.is_some_and(|f| Path::new(f).starts_with(paths::state_dir().join("hooks"))) { n } else { 0 }
}

/// 一个正在跑的 Claude 现在是哪段对话:它自己在配置目录的 `sessions/<pid>.json` 里记着
/// (现在的对话 id —— /clear、压缩之后会换 —— 启动目录、标题);没有这个文件的老版本看命令行里的 `--session-id` / `--resume`
fn live_in(conf: &Path, pid: u32, argv: &[String]) -> Live {
    let own: Value = std::fs::read_to_string(conf.join("sessions").join(format!("{pid}.json")))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(|v: &Value| v["pid"].as_u64() == Some(pid as u64))
        .unwrap_or_default();
    // 分叉着起的(--resume 旧的 --fork-session):命令行里那个是旧对话的 id,不是它现在的
    let from_argv = || {
        let at = argv.iter().position(|a| matches!(a.as_str(), "--session-id" | "--resume" | "-r"))?;
        argv.get(at + 1).filter(|v| !v.starts_with('-') && !argv.iter().any(|a| a == "--fork-session")).cloned()
    };
    let conv = own["sessionId"].as_str().map(String::from).or_else(from_argv).filter(|c| !c.contains('/')).unwrap_or_default();
    let projects = conf.join("projects");
    let file = format!("{conv}.jsonl");
    // 通常就在启动目录对应的文件夹里;名字太长时 Claude 会截断加 hash,那就挨个找
    let transcript = (!conv.is_empty())
        .then(|| {
            let near = own["cwd"].as_str().map(|d| projects.join(project_key(d)).join(&file)).filter(|p| p.is_file());
            near.or_else(|| std::fs::read_dir(&projects).ok()?.flatten().map(|d| d.path().join(&file)).find(|p| p.is_file()))
        })
        .flatten();
    // 没起名字时它自己编一个占位的(目录名加两位:`aca-5e`,nameSource 是 derived),不算标题
    let named = own["name"].as_str().map(str::trim).filter(|n| !n.is_empty() && own["nameSource"] != "derived").map(String::from);
    Live {
        title: named.or_else(|| transcript.as_deref().and_then(transcript_title)).unwrap_or_default(),
        transcript: transcript.map(|t| t.to_string_lossy().into_owned()).unwrap_or_default(),
        conv,
        args: super::carry(argv, not_carried),
    }
}

impl Agent for Claude {
    fn name(&self) -> &str {
        "claude"
    }

    fn home_var(&self) -> Option<&str> {
        Some("CLAUDE_CONFIG_DIR")
    }

    fn default_home(&self) -> Option<&str> {
        Some("~/.claude")
    }

    /// 对话记录 `projects/<项目>/<id>.jsonl`,旁边同名的目录(子代理的记录、存出来的大段工具输出),
    /// 还有 `/rewind` 用的 `file-history/<id>`、任务清单 `tasks/<id>`(有才算)
    fn conv_files(&self, home: &Path, conv: &str) -> Vec<PathBuf> {
        if conv.is_empty() || conv.contains('/') {
            return vec![];
        }
        let file = format!("{conv}.jsonl");
        let Some(dir) = std::fs::read_dir(home.join("projects")).ok().and_then(|d| d.flatten().find(|d| d.path().join(&file).is_file()))
        else {
            return vec![];
        };
        let project = Path::new("projects").join(dir.file_name());
        let mut out = vec![project.join(&file)];
        let extra = [project.join(conv), Path::new("file-history").join(conv), Path::new("tasks").join(conv)];
        out.extend(extra.into_iter().filter(|p| home.join(p).is_dir()));
        out
    }

    fn new_conv(&self) -> String {
        paths::uuid4()
    }

    fn can_fork(&self) -> bool {
        true
    }

    fn argv(&self, tool: &Tool, s: &Session, how: &How, aca: &str) -> Res<Vec<String>> {
        let ours = settings(aca);
        let file = match take_settings(&mut super::extra(tool, s)) {
            None => super::hook_file("claude.json", &ours)?,
            // 合出来的那份按内容起名:几个会话带的不一样也不会互相盖掉
            Some(theirs) => {
                let all = merge(&ours, &theirs, &s.dir)?;
                let mut h = DefaultHasher::new();
                all.hash(&mut h);
                let file = super::hook_file(&format!("claude-{:016x}.json", h.finish()), &all)?;
                // 里面有你那份 settings 的全部内容(可能带着 key):只给自己读
                let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
                file
            }
        };
        let mods = super::claude_mod::offer(tool, &s.id).map(|d| d.to_string_lossy().into_owned());
        // 插件的 MCP 服务:一份按内容起名的 --mcp-config(会话 id 在环境里,不在文件里:同样的插件几个会话共用一份)
        let mcp = match tool.mcp.is_empty() {
            true => None,
            false => {
                let text = crate::plugin::mount::claude_mcp_config(aca, &tool.mcp);
                let mut h = DefaultHasher::new();
                text.hash(&mut h);
                Some(super::hook_file(&format!("mcp-{:016x}.json", h.finish()), &text)?.to_string_lossy().into_owned())
            }
        };
        Ok(plan(tool, s, how, &file.to_string_lossy(), mods.as_deref(), mcp.as_deref()))
    }

    /// 对话记录在才接得上(起了没说过话的会话没有记录);分叉出来没说过话的从原来那段重新分
    fn resume(&self, conv: &str, r: &Run) -> How {
        let t = Path::new(&r.transcript);
        let known = !conv.is_empty() && t.file_stem().is_some_and(|f| f.to_string_lossy() == conv) && t.is_file();
        match (known, r.fork_from.is_empty()) {
            (true, _) => How::Resume(conv.to_string()),
            (false, false) => How::Fork(r.fork_from.clone()),
            (false, true) => How::Fresh,
        }
    }

    fn title_in_pane(&self) -> bool {
        true
    }

    /// 标题不看 hook。先看窗格标题:它一起好就写在那,/rename 之后马上变。那里没有(关了终端标题)才翻对话记录,
    /// 而且只在这几个事件上翻
    fn title(&self, r: &Run, pane_title: &str) -> Option<String> {
        shown_title(pane_title).or_else(|| {
            matches!(r.event.as_str(), "Stop" | "UserPromptSubmit" | "PostCompact")
                .then(|| transcript_title(Path::new(&r.transcript)))
                .flatten()
        })
    }

    fn live_title(&self, _r: &Run, pane_title: &str) -> Option<String> {
        shown_title(pane_title)
    }

    /// 进程名是 claude,或者就是版本号(`2.1.284`:它装的是按版本号命名的文件)
    fn runs_as(&self, comm: &str) -> bool {
        let version = comm.split('.').count() == 3 && comm.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        comm == "claude" || version
    }

    fn last_reply(&self, r: &Run) -> Option<String> {
        transcript_reply(Path::new(&r.transcript))
    }

    fn live(&self, pid: u32, home: Option<&str>, argv: &[String]) -> Live {
        let conf = home.map_or_else(|| PathBuf::from(paths::expand("~/.claude")), PathBuf::from);
        live_in(&conf, pid, argv)
    }

    fn delivery(&self) -> super::Delivery {
        super::Delivery::Bell
    }

    fn prompt_mark(&self) -> Option<char> {
        Some('❯')
    }

    /// 权限框的选项个数和顺序会变(2.1.294 上见过 4 个,「No」排第 4),只做两件事:同意按 1,拒绝按 Esc
    fn permission_keys(&self) -> Option<(&'static str, &'static str)> {
        Some(("1", "Escape"))
    }

    /// 在跑时按一下 Esc 就停(不发任何 hook,连 Stop 都没有)
    fn interrupt_key(&self) -> Option<&'static str> {
        Some("Escape")
    }

    fn opening(&self, text: &str) -> Option<Vec<String>> {
        super::opening_after_dashes(text)
    }

    /// 「Accessing workspace: … ❯ No, exit / Yes, I trust this folder … Enter to confirm · Esc to cancel」。
    /// 2.1.29x 上光标默认停在 No 上(直接回车是退出),早先是「1. Yes, I trust this folder / 2. No, exit」:
    /// 两个选项挨着、光标在其中一个上才认,从光标走到 Yes 再回车
    fn trust_keys(&self, lines: &[String]) -> Option<Vec<&'static str>> {
        let rows: Vec<&str> = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
        let tail = &rows[rows.len().saturating_sub(12)..];
        if !tail.iter().any(|l| l.contains("Enter to confirm")) {
            return None;
        }
        // 选项那一行去掉前面的光标和编号
        let label = |l: &str| -> String {
            l.trim_start_matches('❯').trim_start().trim_start_matches(|c: char| c.is_ascii_digit() || c == '.').trim_start().to_string()
        };
        let yes = tail.iter().position(|l| label(l).starts_with("Yes, I trust this folder"))?;
        let no = tail.iter().position(|l| label(l).starts_with("No, exit"))?;
        if yes.abs_diff(no) != 1 {
            return None;
        }
        let at = [yes, no].into_iter().find(|&i| tail[i].starts_with('❯'))?;
        let mut keys = vec![if yes > at { "Down" } else { "Up" }; yes.abs_diff(at)];
        keys.push("Enter");
        Some(keys)
    }

    /// 屏幕底部:选择框 / 确认框(附它问的那句),或者「✻ Pondering… (1m 12s」那一行(附已跑秒数)
    fn screen(&self, lines: &[String]) -> Seen {
        let nonempty: Vec<&str> = lines.iter().map(|l| l.as_str()).filter(|l| !l.trim().is_empty()).collect();
        let tail = &nonempty[nonempty.len().saturating_sub(25)..];
        let input = input_at(tail);
        // 输入框还在 = 没弹框:正文、命令里碰巧有「Do you want to」这样的句子不算在问
        if input.is_none() && tail[tail.len().saturating_sub(12)..].iter().any(|l| ASK.is_match(l)) {
            let q = tail.iter().map(|l| l.trim()).find(|l| l.ends_with('?') || l.ends_with('？')).unwrap_or("");
            return Seen::Ask(q.to_string());
        }
        if let Some(c) = tail.iter().rev().find_map(|l| RUNNING.captures(l)) {
            return Seen::Running(secs(&c));
        }
        // 输入框头上那条横线再往上的几行
        let near = input.map_or(&[][..], |at| &tail[(at - 1).saturating_sub(NEAR)..at - 1]);
        if let Some(c) = near.iter().rev().find_map(|l| STARTING.captures(l)) {
            return Seen::Running(secs(&c));
        }
        // 往外吐正文时转圈那一行不在底下,只有输入框下面的状态栏写着「esc to interrupt」(2.1.295)
        match input {
            Some(at) if tail[at + 1..].iter().any(|l| l.contains("esc to interrupt")) => Seen::Running(0),
            _ => Seen::Quiet,
        }
    }

    /// 输入框头上那条横线往上 [`BODY`] 行。2.1.300 往外吐正文时没有转圈那一行,状态栏里也没有 esc to interrupt,
    /// 只有这几行在变。行尾空格去掉:界面带颜色抓的屏幕去掉颜色之后,和不带颜色抓的行尾会差几个空格
    fn body(&self, lines: &[String]) -> Option<u64> {
        let nonempty: Vec<&str> = lines.iter().map(|l| l.trim_end()).filter(|l| !l.trim().is_empty()).collect();
        let tail = &nonempty[nonempty.len().saturating_sub(25)..];
        let at = input_at(tail)?;
        let mut h = DefaultHasher::new();
        tail[(at - 1).saturating_sub(BODY)..at - 1].hash(&mut h);
        Some(h.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(conv: &str) -> Session {
        Session {
            id: "ab12".into(),
            tool: "claude".into(),
            conv: conv.into(),
            args: vec!["--model".into(), "haiku".into()],
            ..Default::default()
        }
    }

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn the_folder_trust_question_is_answered_by_walking_to_yes() {
        // 2.1.29x 上抄下来的:光标默认停在 No 上
        let ask = "────────────────\n Accessing workspace:\n\n /p/api\n\n Quick safety check: Is this a project you created or one you trust? (Like your\n own code, a well-known open source project, or work from your team). If not,\n take a moment to review what's in this folder first.\n\n Claude Code'll be able to read, edit, and execute files here.\n\n Security guide\n\n ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel\n";
        assert_eq!(Claude.trust_keys(&lines(ask)), Some(vec!["Down", "Enter"]));
        // 早先的版本:带编号,Yes 在上面、光标就在它上
        let old = " Do you trust the files in this folder?\n\n /p/api\n\n ❯ 1. Yes, I trust this folder\n   2. No, exit\n\n Enter to confirm · Esc to cancel\n";
        assert_eq!(Claude.trust_keys(&lines(old)), Some(vec!["Enter"]));
        // 光标在下面那个 No 上:往上走
        let below = " ❯ 2. No, exit\n".to_string();
        let up = old.replace(" ❯ 1. Yes", "   1. Yes").replace("   2. No, exit\n", &below);
        assert_eq!(Claude.trust_keys(&lines(&up)), Some(vec!["Up", "Enter"]));
        // 别的框、聊到这几个字的正文、画到一半(光标还没出来)的:不按
        let perm = "● 要改 3 个文件\n\n Do you want to make this edit to main.rs?\n ❯ 1. Yes\n   2. Yes, allow all edits during this session\n   3. No\n\n Esc to cancel\n";
        assert_eq!(Claude.trust_keys(&lines(perm)), None);
        assert_eq!(Claude.trust_keys(&lines("● 它会问 Yes, I trust this folder 还是 No, exit\n\n❯ ")), None);
        assert_eq!(Claude.trust_keys(&lines(&ask.replace("❯ No, exit", "  No, exit"))), None);
        assert_eq!(Claude.trust_keys(&lines(&ask.replace(" Enter to confirm · Esc to cancel\n", ""))), None);
        assert_eq!(Claude.trust_keys(&[]), None);
    }

    #[test]
    fn it_starts_resumes_and_forks_with_hooks_passed_at_launch() {
        let t = Tool { args: vec!["-Q".into()], ..Default::default() };
        assert_eq!(
            plan(&t, &s("c-1"), &How::Fresh, "/st/claude.json", None, None).join(" "),
            "claude --session-id c-1 --settings /st/claude.json -Q --model haiku"
        );
        let plain = Tool::default();
        assert_eq!(
            plan(&plain, &s("c-1"), &How::Resume("c-9".into()), "/st/c.json", None, None).join(" "),
            "claude --resume c-9 --settings /st/c.json --model haiku"
        );
        assert_eq!(
            plan(&plain, &s("c-2"), &How::Fork("c-1".into()), "/st/c.json", None, None).join(" "),
            "claude --resume c-1 --fork-session --session-id c-2 --settings /st/c.json --model haiku"
        );
        // 自己定了命令的工具(包一层脚本):参数照样接在后面
        let wrapped = Tool { cmd: vec!["/w/cc.sh".into(), "-x".into()], ..Default::default() };
        assert_eq!(plan(&wrapped, &s(""), &How::Fresh, "/st/c.json", None, None)[..4], ["/w/cc.sh", "-x", "--settings", "/st/c.json"]);
        // aca 的 mod 和 hook 一样是启动时带的:跟在 --settings 后面,你自己的参数还在最后
        assert_eq!(
            plan(&plain, &s("c-1"), &How::Fresh, "/st/c.json", Some("/st/hooks/claude-mod/aca"), None).join(" "),
            "claude --session-id c-1 --settings /st/c.json --plugin-dir /st/hooks/claude-mod/aca --model haiku"
        );
        // 插件的 MCP 服务:--mcp-config 紧跟着 --settings,不会把后面的参数吞成它的
        assert_eq!(
            plan(&plain, &s("c-1"), &How::Fresh, "/st/c.json", None, Some("/st/hooks/mcp-1.json")).join(" "),
            "claude --session-id c-1 --mcp-config /st/hooks/mcp-1.json --settings /st/c.json --model haiku"
        );
    }

    #[test]
    fn your_own_settings_are_merged_with_the_hooks_into_the_one_file_claude_reads() {
        // 工具(账号的参数接在它后面)和会话带的 --settings:两种写法都认,以最后一个为准,别的参数不动
        let t = Tool { args: vec!["--settings=/a/first.json".into(), "-Q".into()], ..Default::default() };
        let mut sess = s("c-1");
        sess.args = vec!["--settings".into(), "mine.json".into(), "--model".into(), "haiku".into()];
        let mut extra = crate::agent::extra(&t, &sess);
        assert_eq!(take_settings(&mut extra).as_deref(), Some("mine.json"));
        assert_eq!(extra, ["-Q", "--model", "haiku"]);
        // 命令里只剩合好的那一份
        assert_eq!(
            plan(&t, &sess, &How::Fresh, "/st/merged.json", None, None).join(" "),
            "claude --session-id c-1 --settings /st/merged.json -Q --model haiku"
        );

        // 你那份里的东西原样留着;同一个事件下你的 hook 在前,aca 的接在后面
        let mine = r#"{"env":{"ANTHROPIC_BASE_URL":"https://x"},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"say done"}]}]}}"#;
        let all: Value = serde_json::from_str(&merge(&settings("/bin/aca"), mine, "/nowhere").unwrap()).unwrap();
        assert_eq!(all["env"]["ANTHROPIC_BASE_URL"], "https://x");
        let stop = all["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(
            (stop.len(), stop[0]["hooks"][0]["command"].as_str(), stop[1]["hooks"][0]["command"].as_str()),
            (2, Some("say done"), Some("'/bin/aca' hook claude"))
        );
        // 状态要的那几个事件,加上门铃
        assert_eq!(all["hooks"].as_object().unwrap().len(), EVENTS.len() + 1);

        // 写的是文件:相对路径按会话的目录找
        let dir = std::env::temp_dir().join(format!("aca-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mine.json"), r#"{"model":"opus"}"#).unwrap();
        let all: Value = serde_json::from_str(&merge(&settings("/bin/aca"), "mine.json", &dir.to_string_lossy()).unwrap()).unwrap();
        assert_eq!((all["model"].as_str(), all["hooks"]["Stop"].as_array().unwrap().len()), (Some("opus"), 1));
        // 读不了、不是 JSON:起之前就说,不带着一份坏的进去
        assert!(merge("{}", "nope.json", &dir.to_string_lossy()).unwrap_err().contains("nope.json"));
        std::fs::write(dir.join("bad.json"), "hooks: []").unwrap();
        assert!(merge("{}", "bad.json", &dir.to_string_lossy()).unwrap_err().contains("JSON"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_settings_file_only_hooks_what_status_and_the_doorbell_need() {
        let v: Value = serde_json::from_str(&settings("/a b/it's/aca")).unwrap();
        let h = v["hooks"].as_object().unwrap();
        assert_eq!(h.len(), EVENTS.len() + 1);
        assert_eq!(h["Stop"][0]["hooks"][0]["command"], r#"'/a b/it'\''s/aca' hook claude"#);
        assert_eq!(h["PreToolUse"][0]["matcher"], "AskUserQuestion");
        assert!(h["Stop"][0].get("matcher").is_none());
        // 门铃:起来时报要盯的文件,文件一变在后台取信,退出码 2 叫醒它
        let start = h["SessionStart"][0]["hooks"].as_array().unwrap();
        assert_eq!((start.len(), start[1]["command"].as_str()), (3, Some(r#"'/a b/it'\''s/aca' inbox watch"#)));
        // 名字:起来时、每发一句时交给它
        assert_eq!(start[2]["command"], r#"'/a b/it'\''s/aca' hook-name"#);
        assert_eq!(h["UserPromptSubmit"][0]["hooks"][1]["command"], r#"'/a b/it'\''s/aca' hook-name"#);
        let take = &h["FileChanged"][0]["hooks"][0];
        assert_eq!((take["command"].as_str(), take["asyncRewake"].as_bool()), (Some(r#"'/a b/it'\''s/aca' inbox take"#), Some(true)));
        assert!(h["FileChanged"][0].get("matcher").is_none());
    }

    #[test]
    fn resuming_needs_a_transcript_for_the_current_conversation() {
        // 起了没说过话:没有对话记录,还用登记的那个 id 新开
        assert_eq!(Claude.resume("c1", &Run::default()), How::Fresh);
        let dir = std::env::temp_dir().join(format!("aca-resume-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = dir.join("c2.jsonl");
        std::fs::write(&t, "{}\n").unwrap();
        let r = Run { transcript: t.to_string_lossy().into_owned(), ..Default::default() };
        assert_eq!(Claude.resume("c2", &r), How::Resume("c2".into()));
        // 记录是别的对话的:不敢接
        assert_eq!(Claude.resume("c3", &r), How::Fresh);
        std::fs::remove_dir_all(&dir).unwrap();
        // 分叉出来还没说过话:自己没有记录,从原来那段重新分
        assert_eq!(Claude.resume("c4", &Run { fork_from: "c1".into(), ..Default::default() }), How::Fork("c1".into()));
    }

    #[test]
    fn the_title_is_the_one_on_the_pane_and_only_its_own() {
        assert_eq!(shown_title("✳ 修登录 bug").as_deref(), Some("修登录 bug"));
        // 干活时转的那几个:现在的、老版本的
        assert_eq!(shown_title("◐ 修登录").as_deref(), Some("修登录"));
        assert_eq!(shown_title("⠂ 修登录").as_deref(), Some("修登录"));
        // 还没起标题
        assert_eq!(shown_title("✳ Claude Code"), None);
        assert_eq!(shown_title("✳ "), None);
        // 不是它写的:tmux 默认的主机名、shell 写的、空的
        assert_eq!(shown_title("my-mac.local"), None);
        assert_eq!(shown_title("~/workspace/app"), None);
        assert_eq!(shown_title("✳修登录"), None);
        assert_eq!(shown_title(""), None);
        // 列表刷新时只问窗格标题,不翻记录
        let stopped = Run { event: "Stop".into(), transcript: "/nonexistent".into(), ..Default::default() };
        assert_eq!(Claude.live_title(&stopped, "✳ 修登录").as_deref(), Some("修登录"));
        assert_eq!(Claude.live_title(&stopped, "my-mac.local"), None);
    }

    #[test]
    fn the_last_reply_is_read_from_the_end_of_the_transcript() {
        let dir = std::env::temp_dir().join(format!("aca-reply-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("t.jsonl");
        let say = |extra: &str, content: &str| {
            format!("{{\"type\":\"assistant\"{extra},\"message\":{{\"model\":\"m\",\"content\":{content}}}}}\n")
        };
        let text = |t: &str| format!("[{{\"type\":\"text\",\"text\":\"{t}\"}}]");
        // 最后一段带字的:后面只调了工具的那条、子 agent 说的、Claude Code 自己垫的都跳过
        let log = [
            say("", &text("先看一下")),
            "{\"type\":\"user\",\"message\":{\"content\":\"\\\"type\\\":\\\"assistant\\\" 这几个字出现在对话里\"}}\n".to_string(),
            say("", &text("跑这条:\\n\\n```sh\\nmake\\n```")),
            say("", "[{\"type\":\"tool_use\",\"name\":\"Bash\"}]"),
            say(",\"isSidechain\":true", &text("子 agent 说的")),
            "{\"type\":\"assistant\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{\"type\":\"text\",\"text\":\"No response requested.\"}]}}\n".to_string(),
        ]
        .concat();
        std::fs::write(&f, log).unwrap();
        let r = Run { transcript: f.to_string_lossy().into_owned(), ..Default::default() };
        assert_eq!(Claude.last_reply(&r).as_deref(), Some("跑这条:\n\n```sh\nmake\n```"));
        // 没有记录、记录里还没有回复
        assert_eq!(Claude.last_reply(&Run::default()), None);
        std::fs::write(&f, "{\"type\":\"user\",\"message\":{\"content\":\"你好\"}}\n").unwrap();
        assert_eq!(Claude.last_reply(&r), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_title_on_the_pane_the_transcript_says_and_your_rename_beats_its_own() {
        let dir = std::env::temp_dir().join(format!("aca-title-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("t.jsonl");
        // 它隔一阵把两条都重写一遍,自己起的那条在后面
        std::fs::write(
            &f,
            "{\"type\":\"user\",\"message\":{\"content\":\"\\\"type\\\":\\\"ai-title\\\" 这几个字出现在对话里\"}}\n\
             {\"type\":\"ai-title\",\"aiTitle\":\"旧的\",\"sessionId\":\"x\"}\n\
             {\"type\":\"custom-title\",\"customTitle\":\"以前改的\",\"sessionId\":\"x\"}\n\
             {\"type\":\"assistant\"}\n\
             {\"type\":\"custom-title\",\"customTitle\":\"修登录 bug\",\"sessionId\":\"x\"}\n\
             {\"type\":\"ai-title\",\"aiTitle\":\"它自己起的\",\"sessionId\":\"x\"}\n\
             {\"type\":\"assistant\"}\n",
        )
        .unwrap();
        let at = |event: &str| Run { event: event.into(), transcript: f.to_string_lossy().into_owned(), ..Default::default() };
        assert_eq!(Claude.title(&at("Stop"), "").as_deref(), Some("修登录 bug"));
        // 窗格标题里有就用那个,不翻记录
        assert_eq!(Claude.title(&at("Stop"), "✳ 窗格上的").as_deref(), Some("窗格上的"));
        assert_eq!(Claude.title(&at("Stop"), "✳ Claude Code").as_deref(), Some("修登录 bug"));
        // 不是每个事件都去翻记录
        assert_eq!(Claude.title(&at("PermissionRequest"), ""), None);
        assert_eq!(Claude.title(&at("PermissionRequest"), "✳ 窗格上的").as_deref(), Some("窗格上的"));
        // 没改过名:它自己起的里最后那条
        std::fs::write(
            &f,
            "{\"type\":\"ai-title\",\"aiTitle\":\"旧的\"}\n{\"type\":\"ai-title\",\"aiTitle\":\"新的\"}\n{\"type\":\"assistant\"}\n",
        )
        .unwrap();
        assert_eq!(Claude.title(&at("Stop"), "").as_deref(), Some("新的"));
        std::fs::write(&f, "{\"type\":\"user\"}\n").unwrap();
        assert_eq!(Claude.title(&at("Stop"), ""), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_running_claude_says_which_conversation_it_is_on() {
        let conf = std::env::temp_dir().join(format!("aca-live-{}", std::process::id()));
        let proj = conf.join("projects/-p-my-app");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(conf.join("sessions")).unwrap();
        std::fs::write(proj.join("c-new.jsonl"), "{\"type\":\"ai-title\",\"aiTitle\":\"记录里的标题\"}\n").unwrap();
        let argv: Vec<String> =
            ["claude", "--session-id", "c-old", "--dangerously-skip-permissions", "--model", "opus"].map(String::from).to_vec();
        // 它自己记的那份:现在的对话(/clear 过,和命令行里的不一样)、启动目录、标题
        std::fs::write(conf.join("sessions/42.json"), r#"{"pid":42,"sessionId":"c-new","cwd":"/p/my.app","name":"修登录"}"#).unwrap();
        let l = live_in(&conf, 42, &argv);
        assert_eq!((l.conv.as_str(), l.title.as_str()), ("c-new", "修登录"));
        assert_eq!(l.transcript, proj.join("c-new.jsonl").to_string_lossy());
        // 参数照搬,接哪段对话的那两个词不搬
        assert_eq!(l.args.as_deref(), Some(&["--dangerously-skip-permissions", "--model", "opus"].map(String::from)[..]));
        // 它自己编的占位名不算标题:用对话记录里的
        std::fs::write(
            conf.join("sessions/42.json"),
            r#"{"pid":42,"sessionId":"c-new","cwd":"/p/my.app","name":"my-app-5e","nameSource":"derived"}"#,
        )
        .unwrap();
        assert_eq!(live_in(&conf, 42, &argv).title, "记录里的标题");
        // 没起名字的:用对话记录里它自己起的;启动目录对不上文件夹名(名字太长被截过)就挨个找
        std::fs::write(conf.join("sessions/42.json"), r#"{"pid":42,"sessionId":"c-new","cwd":"/elsewhere"}"#).unwrap();
        let l = live_in(&conf, 42, &argv);
        assert_eq!((l.title.as_str(), l.transcript.is_empty()), ("记录里的标题", false));
        // 那份文件是别的进程留下的(pid 被重新用了)、或者没有:看命令行;没说过话就没有对话记录
        std::fs::write(conf.join("sessions/43.json"), r#"{"pid":7,"sessionId":"c-new"}"#).unwrap();
        let l = live_in(&conf, 43, &argv);
        assert_eq!((l.conv.as_str(), l.transcript.as_str(), l.title.as_str()), ("c-old", "", ""));
        // 分叉着起的:命令行里那个是旧对话
        let forked: Vec<String> = ["claude", "--resume", "c-old", "--fork-session"].map(String::from).to_vec();
        assert_eq!(live_in(&conf, 44, &forked), Live { args: Some(vec![]), ..Default::default() });
        std::fs::remove_dir_all(&conf).unwrap();
    }

    #[test]
    fn only_switches_and_single_words_are_carried_over() {
        let carried = |words: &[&str]| crate::agent::carry(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>(), not_carried);
        let some = |words: &[&str]| Some(words.iter().map(|w| w.to_string()).collect::<Vec<_>>());
        assert_eq!(carried(&["claude"]), some(&[]));
        assert_eq!(
            carried(&["claude", "--resume", "c-1", "--model=opus", "-r=c-2", "--continue", "--verbose"]),
            some(&["--model=opus", "--verbose"])
        );
        // --resume 后面没跟 id(它自己弹出来挑):只丢这一个词
        assert_eq!(carried(&["claude", "--resume", "--model", "opus"]), some(&["--model", "opus"]));
        // aca 自己带进去的那份 hook 不搬;你自己的 --settings 照搬(起的时候和 aca 的合成一份)
        let ours = paths::state_dir().join("hooks/claude.json").to_string_lossy().into_owned();
        assert_eq!(carried(&["claude", "--settings", &ours, "--settings", "mine.json"]), some(&["--settings", "mine.json"]));
        assert_eq!(carried(&["claude", &format!("--settings={ours}")]), some(&[]));
        // aca 的 mod 也不搬;你自己带的 --plugin-dir 照搬
        let mods = paths::state_dir().join("hooks/claude-mod/aca").to_string_lossy().into_owned();
        assert_eq!(carried(&["claude", "--plugin-dir", &mods, "--plugin-dir", "/my/mod"]), some(&["--plugin-dir", "/my/mod"]));
        // 起的时候就交给它一句话的:认不准哪些是参数,整个不搬
        assert_eq!(carried(&["claude", "--dangerously-skip-permissions", "修一下 登录"]), None);
        assert_eq!(carried(&["claude", "fix"]), None);
        assert_eq!(carried(&["claude", "--model", "opus", "fix"]), None);
        assert_eq!(carried(&[]), None);
    }

    #[test]
    fn the_process_is_named_claude_or_just_its_version() {
        assert!(Claude.runs_as("claude") && Claude.runs_as("2.1.284"));
        // 桌面 App(`Claude`、`Claude Helper`)不是
        assert!(!Claude.runs_as("1.2") && !Claude.runs_as("a.b.c") && !Claude.runs_as("Claude") && !Claude.runs_as("Claude Helper"));
        assert_eq!(
            (crate::agent::by_comm("2.1.284"), crate::agent::by_comm("codex"), crate::agent::by_comm("zsh")),
            (Some("claude"), Some("codex"), None)
        );
    }

    #[test]
    fn a_dialog_is_asking_and_its_question_is_the_line_ending_in_a_question_mark() {
        let scr = lines("● 要改 3 个文件\n\n Do you want to make this edit to main.rs?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel");
        assert_eq!(Claude.screen(&scr), Seen::Ask("Do you want to make this edit to main.rs?".into()));
    }

    #[test]
    fn the_activity_line_means_running_and_gives_the_elapsed_time() {
        assert_eq!(Claude.screen(&lines("● 读文件\n\n✻ Pondering… (1m 12s · ↓ 2.1k tokens)\n\n❯ ")), Seen::Running(72));
        assert_eq!(Claude.screen(&lines("✶ Cooking… (2h 3m 4s)")), Seen::Running(7384));
        assert_eq!(Claude.screen(&lines("● 做完了\n\n❯ ")), Seen::Quiet);
        assert_eq!(Claude.screen(&[]), Seen::Quiet);
    }

    /// 输入框和它底下那几行(2.1.295 的样子);`title` 是横线右边嵌着的标题
    fn with_input(above: &str, title: &str) -> Vec<String> {
        let bar = "─".repeat(40);
        lines(&format!(
            "{above}\n{bar}{title}\n❯ \n{bar}\n  ━━╌╌ 20% │ Opus 5.5 │ aca\n  $56.854 6446m53s\n  ⏵⏵ bypass permissions on (shift+tab to cycle)"
        ))
    }

    #[test]
    fn a_turn_that_just_started_has_no_timer_yet_and_still_counts_as_running() {
        // 发出去头十几秒(模型还没回第一个字)就这一行,下面夹一行提示
        let tip = "  ⎿  Tip: Run /install-github-app to tag @claude right from your Github issues and PRs";
        assert_eq!(Claude.screen(&with_input(&format!("❯ 接着改\n· Combobulating…\n{tip}"), "")), Seen::Running(0));
        assert_eq!(Claude.screen(&with_input("❯ 接着改\n✢ Nucleating…", " 修登录 ─")), Seen::Running(0));
        // 有计时了照旧
        assert_eq!(Claude.screen(&with_input("· Combobulating… (16s)", "")), Seen::Running(16));
        assert_eq!(Claude.screen(&with_input("✻ Compacting conversation… (1m 5s · esc to interrupt)", "")), Seen::Running(65));
        // 跑完的那一行没有省略号;正文里省略号结尾的行不是那几个字符开头的,或者离输入框远
        assert_eq!(Claude.screen(&with_input("  都改好了。\n✻ Cooked for 13m 36s · done 10:48 AM", "")), Seen::Quiet);
        assert_eq!(Claude.screen(&with_input("⏺ 先这样…\n  - 剩下的回头再说…", "")), Seen::Quiet);
        let far = format!("· 顺手记一笔…\n{}", ["  正文"; 7].join("\n"));
        assert_eq!(Claude.screen(&with_input(&far, "")), Seen::Quiet);
        // 没有输入框(认不出这一版的样子):不猜
        assert_eq!(Claude.screen(&lines("· Combobulating…\n❯ ")), Seen::Quiet);
    }

    #[test]
    fn while_it_streams_text_only_the_status_bar_says_it_is_running() {
        // 2.1.295 往外吐正文时抓的:底下没有转圈那一行,只有输入框下面的状态栏写着 esc to interrupt
        let bar = "─".repeat(40);
        let streaming = format!(
            "  have emerged across the Red Sea in Yemen.\n  Yemen and the Sufi Connection\n  By the fifteenth century, coffee was being cultivated…\n\
             {bar}\n❯ \n{bar}\n  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt · ← 1 agent"
        );
        assert_eq!(Claude.screen(&lines(&streaming)), Seen::Running(0));
        // 跑完了:状态栏里没有这一句(后台还挂着一个 shell 也不算)
        let after = format!(
            "⏺ 写完了。\n✻ Worked for 9s · done 2:12 PM · 1 shell still running\n{bar}\n❯ \n{bar}\n  ⏵⏵ auto mode on · 1 shell · ← 1 agent · ↓ to manage"
        );
        assert_eq!(Claude.screen(&lines(&after)), Seen::Quiet);
        // 正文里出现这几个字不算:只看输入框下面
        assert_eq!(Claude.screen(&with_input("⏺ 按 esc to interrupt 可以停", "")), Seen::Quiet);
    }

    /// 2.1.300 往外吐正文时的样子(2026-10-16 本机抓的,字换了):没有转圈那一行,状态栏里没有 esc to interrupt,
    /// 窗格标题也是 ✳。`text` 是正文的尾巴,`cost` 是状态栏里会自己刷新的那一段,`input` 是输入框里打着的字
    fn streaming(text: &str, cost: &str, input: &str) -> Vec<String> {
        let bar = "─".repeat(40);
        lines(&format!(
            "⏺ 两条路的差别在于:\n{text}\n{bar} 修登录 ─\n❯ {input}\n{bar}\n  ━╌╌╌╌ 11% │ Opus 5.5 │ aca\n  {cost} 40m41s\n  \
             ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 1 agent"
        ))
    }

    #[test]
    fn on_2_1_300_text_coming_out_shows_only_as_the_lines_above_the_input_changing() {
        let a = streaming("  - 第一条", "$1.841", "");
        let b = streaming("  - 第一条\n  - 第二条", "$1.841", "");
        // 屏幕本身看不出在跑
        assert_eq!((Claude.screen(&a), Claude.screen(&b)), (Seen::Quiet, Seen::Quiet));
        // 正文长了一行:指纹变了
        assert!(Claude.body(&a).is_some() && Claude.body(&a) != Claude.body(&b));
        // 输入框底下的状态栏刷新(闲着也隔 10 / 60 秒一次)、输入框里打字、行尾多了空格:都不算
        for same in
            [streaming("  - 第一条", "$1.902", ""), streaming("  - 第一条", "$1.841", "接着改"), streaming("  - 第一条   ", "$1.841", "")]
        {
            assert_eq!(Claude.body(&same), Claude.body(&a));
        }
        // 弹了框、输入框收起来了:认不出,不看
        assert_eq!(Claude.body(&lines("Do you want to proceed?\n❯ 1. Yes\n  2. No")), None);
    }

    #[test]
    fn asking_means_the_dialog_replaced_the_input_box() {
        let bar = "─".repeat(40);
        // 权限确认框
        let allow = format!(
            "⏺ Bash(touch hello.txt)\n  ⎿  Waiting…\n{bar}\n Bash command\n touch hello.txt\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel · Tab to amend"
        );
        assert_eq!(Claude.screen(&lines(&allow)), Seen::Ask("Do you want to proceed?".into()));
        // AskUserQuestion:光标挪到最后一项时,它头上正好是一条横线,脚下没有
        let pick = format!(
            "{bar}\n ☐ Favorite\nWhat is your favorite color?\n  1. Red\n  2. Blue\n  3. Type something.\n{bar}\n❯ 4. Chat about this\nEnter to select · ↑/↓ to navigate · Esc to cancel"
        );
        assert_eq!(Claude.screen(&lines(&pick)), Seen::Ask("What is your favorite color?".into()));
        // 输入框还在:回复、命令里有这几个词不算在问,该在跑还是在跑
        assert_eq!(Claude.screen(&with_input("⏺ 改好了。Would you like to run the tests?", "")), Seen::Quiet);
        let cmd = "⏺ Bash(grep -n 'Do you want to|esc to cancel' src/data.rs)\n  ⎿  Running…\n✽ Hashing… (6m 6s · ↓ 28.4k tokens)";
        assert_eq!(Claude.screen(&with_input(cmd, "")), Seen::Running(366));
    }
}
