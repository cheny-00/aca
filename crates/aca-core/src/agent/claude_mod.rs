//! aca 带进 Claude 的 mod:在 Claude Code 自己的进程里跑的一小段代码(它叫 function hooks,2.1.295 上有;
//! 接口还是 early access,每一版都可能变)。和 hook 一样**启动时带进去**(`--plugin-dir <state>/hooks/claude-mod/aca`),
//! 不装进任何配置目录。开关是 `[tools.claude] mod = true`,不写就不带。
//!
//! 现在只做一件事:Claude 每次量出用量(`session.measure`:一起来、每轮结束、额度每动一个点)就原样交给
//! `aca limits --mod`,订阅额度不用再靠它配置目录里的 statusLine。statusLine 那条路原样留着,
//! 这边坏了最多是回到没带的样子。
//!
//! 接口会变,所以:
//! - **mod 只转发**(`claude_mod.js`):一个事件、一个 `$` 调用。认格式在 aca 这边(aca-tui 的 `limits.rs`),变了改 Rust 不改它
//! - **起之前让这一版 Claude 自己验**(`claude plugin validate --json`,0.6 秒):按程序文件记一次,不过的这一版就不带。
//!   事件改名、没了它验得出;`$` 上的调用改名、事件里的字段改名它验不出(2.1.295 上试过),靠下面这条
//! - **对账**:带着起的会话跑完过一轮(hook 记的 `done`,那条路是稳的)却一次都没交来,而且这一版从没有哪个会话交来过
//!   → 记这一版不报数,之后不带。交来了但认不出的,接收那头当场记([`heard`])
//!
//! 记在 `<state>/mod/`:`builds/<这一版的记号>.json` 是结论,`up/<会话>` 是它带着哪一版、什么时候起的,
//! `heard/<会话>` 是它交来过。Claude 自己还会往 mod 的目录里放一份类型声明(`.claude-plugin/types/`,不到 1 MB)。

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Tool;
use crate::{paths, runtime};
use crate::{t, tr};

const MANIFEST: &str = "{ \"name\": \"aca\", \"version\": \"0.1.0\", \"description\": \"aca 起会话时带进来的:把用量交给 aca 记下\", \"author\": { \"name\": \"aca\" } }\n";
const HOOKS: &str = "{ \"modules\": [\"./register.js\"] }\n";
const MODULE: &str = include_str!("claude_mod.js");
/// mod 挂的那个事件:验的时候要看到这一版真的认了它
const EVENT: &str = "session.measure";
/// 一起来 Claude 就量一次(实测 2 秒内交来);起了这么久、跑完过一轮还没交来的才算数
const GRACE: u64 = 20;
const TIMEOUT: Duration = Duration::from_secs(5);
/// 没了的会话留下的 `up` / `heard`,过这么久删掉
const STALE: u64 = 30 * 86400;

/// 一版 Claude 带不带这个 mod
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Verdict {
    /// 程序文件的名字:它装成按版本号命名的文件,所以通常就是版本号
    pub build: String,
    pub ok: bool,
    /// 不带的原因
    pub why: String,
    pub at: u64,
}

fn root() -> PathBuf {
    paths::state_dir().join("mod")
}

fn verdict_file(key: &str) -> PathBuf {
    root().join("builds").join(format!("{key}.json"))
}

fn mark(dir: &str, id: &str) -> PathBuf {
    root().join(dir).join(id)
}

/// `up/<会话>` 里记的:(哪一版, 什么时候起的)
fn up(id: &str) -> Option<(String, u64)> {
    let text = std::fs::read_to_string(mark("up", id)).ok()?;
    let (key, at) = text.trim().split_once(' ')?;
    Some((key.to_string(), at.parse().ok()?))
}

fn heard_from(id: &str) -> String {
    std::fs::read_to_string(mark("heard", id)).unwrap_or_default().trim().to_string()
}

fn load(key: &str) -> Option<Verdict> {
    serde_json::from_str(&std::fs::read_to_string(verdict_file(key)).ok()?).ok()
}

fn save(key: &str, v: &Verdict) {
    let _ = paths::write_atomic(&verdict_file(key), &serde_json::to_string(v).unwrap_or_default());
}

/// 这一版的结论改成不带(原来记着能带的才改:头一个原因留着)
fn refuse(key: &str, why: &str) {
    if let Some(v) = load(key).filter(|v| v.ok) {
        save(key, &Verdict { ok: false, why: why.into(), at: paths::now(), ..v });
    }
}

/// 这一版 Claude 的记号:程序文件(链接指到的那个)的路径、大小、修改时间,和 mod 自己的内容。
/// 它升级了、mod 改了,记号就变,重新验。第二个是给人看的名字
fn build(program: &str) -> Option<(String, String)> {
    let real = std::fs::canonicalize(program).ok()?;
    let meta = std::fs::metadata(&real).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let mut h = DefaultHasher::new();
    (&real, meta.len(), mtime, MODULE, HOOKS, MANIFEST).hash(&mut h);
    Some((format!("{:016x}", h.finish()), real.file_name()?.to_string_lossy().into_owned()))
}

/// `claude plugin validate --json` 的报告 → 这一版认不认这个 mod。光它说过了不够,还要它真的列出了那个事件:
/// 不认 function hooks 的版本不看 hooks.json 里的 `modules`,也就不会报错
fn judge(report: &Value) -> Result<(), String> {
    let parts: Vec<&Value> = std::iter::once(&report["manifest"]).chain(report["contents"].as_array().into_iter().flatten()).collect();
    let list = |key: &str| parts.iter().flat_map(move |p| p[key].as_array().into_iter().flatten()).collect::<Vec<_>>();
    if let Some(e) = list("errors").first() {
        // 报错里带着文件的完整路径,只留最后说事的那一段
        let msg = e["message"].as_str().unwrap_or("").split(';').next().unwrap_or("");
        return Err(msg.rsplit(": ").next().unwrap_or(msg).trim().to_string());
    }
    if report["success"] != true {
        return Err(t!("plugin validate 未通过", "its plugin validate didn't say it passed").into());
    }
    let hooked = list("notes").iter().filter_map(|n| n.as_str()).any(|n| n.contains("hooks:") && n.contains(EVENT));
    hooked.then_some(()).ok_or_else(|| tr!("此版本无法识别 {EVENT} hook", "this version didn't recognize the {EVENT} hook"))
}

/// 让这一版 Claude 验一遍。给它一个只放这次验证的配置目录:它一跑就往配置目录里写 `.claude.json`,不让它写到你的那几个里
fn validate(program: &str, dir: &Path) -> Result<(), String> {
    let mut cmd = Command::new(program);
    cmd.args(["plugin", "validate", "--json"]).arg(dir).env("CLAUDE_CONFIG_DIR", root().join("validate-home"));
    for k in super::FOREIGN {
        cmd.env_remove(k);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| tr!("无法运行 {program}:{e}", "can't run {program}: {e}"))?;
    let mut out = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = out.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + TIMEOUT;
    while !matches!(child.try_wait(), Ok(Some(_))) {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(tr!("验证时 {} 秒内无响应", "no answer within {} seconds while validating", TIMEOUT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let report: Value = serde_json::from_str(&reader.join().unwrap_or_default())
        .map_err(|_| t!("plugin validate 未返回报告", "its plugin validate gave no report").to_string())?;
    judge(&report)
}

/// mod 的那个目录:写好(内容没变就不动 —— 开着的会话盯着这个目录,一动就重新加载),返回路径
fn write() -> Option<PathBuf> {
    let at = |name: &str| format!("claude-mod/aca/{name}");
    let manifest = super::hook_file(&at(".claude-plugin/plugin.json"), MANIFEST).ok()?;
    super::hook_file(&at("hooks/hooks.json"), HOOKS).ok()?;
    super::hook_file(&at("hooks/register.js"), MODULE).ok()?;
    Some(manifest.parent()?.parent()?.to_path_buf())
}

/// 对账的那一半,纯的:`ups` 是带着这一版起的会话(起的时刻、最近一次跑完的时刻、交没交来过)。
/// 有会话起了够久、之后跑完过一轮、没交来,而且没有一个会话交来过 = 这一版不报数
fn silent(ups: &[(u64, u64, bool)]) -> bool {
    ups.iter().all(|u| !u.2) && ups.iter().any(|&(at, done, _)| done >= at + GRACE)
}

/// 翻一遍 `up/`:这一版是不是不报数;顺手删掉早没人管的
fn reconcile(key: &str, now: u64) -> bool {
    let Ok(rd) = std::fs::read_dir(root().join("up")) else { return false };
    let mut ups = vec![];
    for e in rd.flatten() {
        let id = e.file_name().to_string_lossy().into_owned();
        let Some((k, at)) = up(&id) else { continue };
        if now.saturating_sub(at) > STALE && !paths::run_file(&id).exists() {
            let _ = std::fs::remove_file(e.path());
            let _ = std::fs::remove_file(mark("heard", &id));
        } else if k == key {
            ups.push((at, runtime::load(&id).done, heard_from(&id) == key));
        }
    }
    silent(&ups)
}

/// 起这个会话时带不带 mod:带的话返回它的目录,并记下这个会话是带着哪一版起的。
/// 不带的几种:没开;工具自己包了一层命令(不知道里面跑的是哪一版,结论没法按版本记);这一版验不过;这一版对过账不报数
pub(crate) fn offer(tool: &Tool, id: &str) -> Option<PathBuf> {
    let found = offered(tool);
    let now = paths::now();
    let _ = match &found {
        Some((_, key)) => paths::write_atomic(&mark("up", id), &format!("{key} {now}\n")),
        None => std::fs::remove_file(mark("up", id)),
    };
    found.map(|(dir, _)| dir)
}

fn offered(tool: &Tool) -> Option<(PathBuf, String)> {
    if !tool.mods || !tool.cmd.is_empty() {
        return None;
    }
    let program = super::which("claude")?;
    let (key, name) = build(&program)?;
    let dir = write()?;
    let now = paths::now();
    let verdict = load(&key).unwrap_or_else(|| {
        let checked = validate(&program, &dir);
        let v = Verdict { build: name, ok: checked.is_ok(), why: checked.err().unwrap_or_default(), at: now };
        save(&key, &v);
        v
    });
    if verdict.ok && reconcile(&key, now) {
        refuse(&key, t!("带它的会话已完成一轮,但未收到任何用量", "a session carrying it finished a turn without reporting any usage"));
        return None;
    }
    verdict.ok.then_some((dir, key))
}

/// 接收那头(`aca limits --mod`)报:这个会话的 mod 交来了一次。`understood` = 交来的东西认得出;
/// 认不出就是事件的形状变了,这一版之后不带(带着也白带)
pub fn heard(id: &str, understood: bool) {
    let Some((key, _)) = up(id) else { return };
    if heard_from(id) != key {
        let _ = paths::write_atomic(&mark("heard", id), &format!("{key}\n"));
    }
    if !understood {
        refuse(&key, t!("无法识别它上报的用量(事件格式已变)", "the usage it reports is no longer recognized (the event shape changed)"));
    }
}

/// 给人看的:现在 PATH 里这一版 Claude 的结论(还没验过 = None)
pub fn verdict() -> Option<Verdict> {
    load(&build(&super::which("claude")?)?.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_build_takes_the_mod_only_when_its_own_validator_lists_the_hook() {
        // 2.1.295 的 `claude plugin validate --json` 原样的报告(路径缩短了)
        let passed = json!({"success": true, "manifest": {"errors": [], "warnings": [{"path": "author", "message": "No author"}], "notes": []},
            "contents": [{"type": "hooks", "errors": [], "notes": ["./register.js hooks: session.measure", "./register.js calls: $.process.run"]}]});
        assert_eq!(judge(&passed), Ok(()));
        // 事件改了名:它自己说不是事件
        let renamed = json!({"success": false, "manifest": {"errors": []}, "contents": [{"errors": [{"path": "modules../register.js",
            "message": "aca: /st/hooks/claude-mod/hooks/register.js:5: \"session.measure\" is not an event; $ is always spelled $.noun.event(...)"}], "notes": []}]});
        assert_eq!(judge(&renamed).unwrap_err(), "\"session.measure\" is not an event");
        // 不认 function hooks 的版本:清单没毛病就算过,但没列出那个事件
        let old = json!({"success": true, "manifest": {"errors": []}, "contents": []});
        assert!(judge(&old).unwrap_err().contains("session.measure"));
        // 不是报告(老版本没有这条命令,打的是用法)
        assert!(judge(&json!({})).is_err());
    }

    #[test]
    fn a_build_is_silent_when_turns_finish_and_nothing_ever_arrives() {
        // 刚起、还没跑完过:不算
        assert!(!silent(&[(100, 0, false)]));
        // 起了 5 秒就跑完了一轮(接着原来的对话起的):mod 可能还没来得及交,不算
        assert!(!silent(&[(100, 105, false)]));
        // 跑完了一轮,一次都没交来
        assert!(silent(&[(100, 160, false)]));
        // 别的会话交来过:这一版是通的,是那个会话自己的事
        assert!(!silent(&[(100, 160, false), (90, 0, true)]));
        assert!(!silent(&[]));
    }

    #[test]
    fn the_shipped_module_stays_a_plain_forwarder() {
        // 验的时候看的就是这两样;多挂一个事件、多用一个 `$` 调用,都多一处会跟着接口变
        assert_eq!(MODULE.matches("on('").count(), 1);
        assert!(MODULE.contains(&format!("on('{EVENT}'")));
        assert_eq!(MODULE.matches("$.").count(), 1);
        assert!(serde_json::from_str::<Value>(MANIFEST).is_ok() && serde_json::from_str::<Value>(HOOKS).is_ok());
    }
}
