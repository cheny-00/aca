//! 收件箱:会话之间递话。每个会话一个 `inbox/<id>.jsonl`,一行一条,只往后加;哪几条送到了记在旁边的 `<id>.state`。
//!
//! 谁往里放:`aca msg`(人、agent、脚本),和 worker 自己的 hook —— 它跑完一轮、出错、退出、在等确认时,
//! 给派它的那个(`parent`)放一条通知([`notify`])。通知只往上发:派活关系是一棵树,不会两个 agent 互相叫醒停不下来。
//!
//! 怎么送进去看收信的那一家([`Delivery`]),前两种都不碰输入框:
//! - **门铃**(Claude):改一下 `inbox/<id>.bell`。Claude 起来时 `aca inbox watch` 让它盯着这个文件,
//!   一变它就在后台跑 `aca inbox take`:取出没送的,写到 stderr、退出码 2,Claude 当成系统提示给模型
//!   (空着就开一轮,在跑就塞进这一轮,在等你就等你答完)。按了 10 秒还没人取,就当这次没接上,改用打字
//! - **排队**(Codex):`codex queue --thread <对话> --message <那段话>`,Codex 窗口自己每 10 秒看一次队列,
//!   空着就开一轮。它在跑、在等你确认、刚被你按 Esc 打断时先不排,等它自己这一轮跑完(Stop hook)再排
//! - **打字**:前两样用不了时(没接上门铃的 Claude、还没说过话的 Codex、别的 agent)。对方空着、
//!   你没在那一格打字、输入框里没有你没发的字,才粘进去按回车;不然 20 秒后再看一次,还不行等它下次的 hook
//!
//! 没有常驻进程:发的那一刻送一次([`deliver`]);送不了的留着,等对方自己的 hook 下次跑(一轮跑完、重新起来)再送。
//! 屏幕上像在跑、hook 却没说有一轮在跑的(刚起来在加载),20 秒后自己再看一次;`aca start` 起来后过几秒也送一次
//! (Codex 的 SessionStart 要等第一句话才报)。
//! 每放进一条、每送到一条,事件流里记一行(`message.sent` / `message.delivered`)。
//!
//! 不开口就不往 agent 里加东西:没人发话时,收信那一头的上下文里没有 aca 的任何字;worker 跑完要不要叫醒派它的那个,
//! 派的时候说(`aca new --notify`),不说不叫。

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::{self, Agent, Delivery};
use crate::config::Config;
use crate::paths::{self, Res};
use crate::runtime::{self, Run, clip};
use crate::status::{self, State};
use crate::store::{Session, Store};
use crate::{event, tmux};

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Msg {
    pub id: String,
    pub ts: u64,
    /// 谁发的:会话 id;不是在会话里发的是 `you`
    pub from: String,
    /// 发的那个给人看的样子:`ab12「修登录」claude:sub`
    pub who: String,
    pub to: String,
    /// msg = 有人发的话;notice = worker 的 hook 自动发的通知
    pub kind: String,
    /// 通知说的是哪件事:turn.done / turn.failed / agent.exited / agent.waiting
    pub what: String,
    /// 要不要为它叫醒对方;不叫醒的,等下次送别的时一起带上
    pub wake: bool,
    pub text: String,
}

/// 哪几条送到了:`got` 已经到了对方眼前;`queued` 排进了 Codex 的队列,还没看到它为这几条开一轮
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
struct Ledger {
    got: Vec<String>,
    queued: Vec<String>,
}

/// 一条消息走到哪了
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Pending,
    Queued,
    Got,
}

impl Stage {
    pub fn said(self) -> &'static str {
        match self {
            Stage::Pending => "没送",
            Stage::Queued => "排着",
            Stage::Got => "送到了",
        }
    }
}

/// 送一次的结果
#[derive(Clone, Debug, PartialEq)]
pub enum Sent {
    /// 没有要叫醒它的
    Nothing,
    Bell,
    Queued,
    Typed,
    /// 先留着,为什么
    Held(String),
}

impl Sent {
    pub fn said(&self) -> String {
        match self {
            Sent::Nothing => "没有要叫醒它的".into(),
            Sent::Bell => "按了门铃".into(),
            Sent::Queued => "排进了它的队列,它空下来自己开一轮".into(),
            Sent::Typed => "打字送进去了".into(),
            Sent::Held(why) => format!("先存着:{why}"),
        }
    }
}

/// 同一对会话 10 分钟内到了这么多条,后面的只存不叫醒
const BRAKE: usize = 20;
const BRAKE_SECS: u64 = 600;
/// 文件到这么多条就收拾一次:没送的全留,送到了的只留最近这么多条
const TIDY_AT: usize = 500;
const KEEP_DONE: usize = 100;
/// 你在那一格最后一次按键在这么多秒以内,就当你在打字
const TYPING_SECS: u64 = 15;
/// 按了门铃这么久还没人取:这次起来的 Claude 没接上门铃
const BELL_CHECK_SECS: u64 = 10;
/// 打字送不了(你在打字、有草稿)时,过这么久再看一次
const RETRY_SECS: u64 = 20;

fn file(id: &str) -> PathBuf {
    paths::inbox_dir().join(format!("{id}.jsonl"))
}

fn ledger_file(id: &str) -> PathBuf {
    paths::inbox_dir().join(format!("{id}.state"))
}

pub fn bell(id: &str) -> PathBuf {
    paths::inbox_dir().join(format!("{id}.bell"))
}

/// 这个收件箱的锁:放、取、记送到都在锁里,两个进程同时取,只有一个取得到
fn lock(id: &str) -> Option<std::fs::File> {
    let _ = std::fs::create_dir_all(paths::inbox_dir());
    let f = std::fs::OpenOptions::new().create(true).append(true).open(paths::inbox_dir().join(format!("{id}.lock"))).ok()?;
    let _ = f.lock();
    Some(f)
}

pub fn all(id: &str) -> Vec<Msg> {
    std::fs::read_to_string(file(id)).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn ledger(id: &str) -> Ledger {
    std::fs::read_to_string(ledger_file(id)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(id: &str, l: &Ledger) {
    let _ = paths::write_atomic(&ledger_file(id), &serde_json::to_string(l).unwrap_or_default());
}

fn stage(l: &Ledger, m: &Msg) -> Stage {
    if l.got.contains(&m.id) {
        Stage::Got
    } else if l.queued.contains(&m.id) {
        Stage::Queued
    } else {
        Stage::Pending
    }
}

/// 全部消息,带着各自走到哪了(`aca inbox`)
pub fn list(id: &str) -> Vec<(Msg, Stage)> {
    let l = ledger(id);
    all(id)
        .into_iter()
        .map(|m| {
            let st = stage(&l, &m);
            (m, st)
        })
        .collect()
}

/// 还没送的:没到对方眼前,也没排进队列
pub fn pending(id: &str) -> Vec<Msg> {
    list(id).into_iter().filter(|(_, s)| *s == Stage::Pending).map(|(m, _)| m).collect()
}

/// 有没有该叫醒它的
pub fn wants_wake(id: &str) -> bool {
    pending(id).iter().any(|m| m.wake)
}

/// 放一条进 `m.to` 的收件箱:补上 id 和时刻;同一个人 10 分钟里说了太多的,只存不叫醒。返回放进去的那条
pub fn post(mut m: Msg) -> Msg {
    m.id = format!("m{}", paths::short_id());
    m.ts = paths::now();
    let braked = {
        let _l = lock(&m.to);
        let mut msgs = all(&m.to);
        let recent = msgs.iter().filter(|x| x.from == m.from && m.ts.saturating_sub(x.ts) < BRAKE_SECS).count();
        let braked = m.wake && recent >= BRAKE;
        m.wake &= !braked;
        msgs.push(m.clone());
        if msgs.len() >= TIDY_AT {
            tidy(&m.to, msgs);
        } else {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(file(&m.to)) {
                let _ = f.write_all(format!("{}\n", serde_json::to_string(&m).unwrap_or_default()).as_bytes());
            }
        }
        braked && recent == BRAKE
    };
    let what = if m.what.is_empty() { Value::Null } else { json!(m.what) };
    event::emit(
        "message.sent",
        &m.to,
        json!({ "id": m.id, "from": m.from, "kind": m.kind, "what": what, "wake": m.wake, "text": clip(&m.text, 200) }),
    );
    // 刚踩到刹车的那一条:告诉你一声(之后的不再说)
    if braked {
        event::emit("message.throttled", &m.to, json!({ "from": m.from, "count": BRAKE, "secs": BRAKE_SECS }));
    }
    m
}

/// 收拾:没送的全留,送到了的只留最近的几条;记着的送到了的 id 也只留还在的(在锁里调)
fn tidy(id: &str, msgs: Vec<Msg>) {
    let mut l = ledger(id);
    let done: Vec<usize> = msgs.iter().enumerate().filter(|(_, m)| stage(&l, m) != Stage::Pending).map(|(i, _)| i).collect();
    let drop: std::collections::HashSet<usize> = done[..done.len().saturating_sub(KEEP_DONE)].iter().copied().collect();
    let kept: Vec<Msg> = msgs.into_iter().enumerate().filter(|(i, _)| !drop.contains(i)).map(|(_, m)| m).collect();
    let text: String = kept.iter().map(|m| format!("{}\n", serde_json::to_string(m).unwrap_or_default())).collect();
    let _ = paths::write_atomic(&file(id), &text);
    l.got.retain(|x| kept.iter().any(|m| &m.id == x));
    l.queued.retain(|x| kept.iter().any(|m| &m.id == x));
    save(id, &l);
}

/// 记成送到了(在锁里调),每条发一个 `message.delivered`;已经记过的不再发
fn mark_locked(id: &str, ids: &[String], how: &str) {
    let mut l = ledger(id);
    let fresh: Vec<&String> = ids.iter().filter(|x| !l.got.contains(x)).collect();
    if fresh.is_empty() {
        return;
    }
    l.got.extend(fresh.iter().map(|x| x.to_string()));
    l.queued.retain(|x| !ids.contains(x));
    save(id, &l);
    for x in fresh {
        event::emit("message.delivered", id, json!({ "id": x, "how": how }));
    }
}

pub fn mark(id: &str, ids: &[String], how: &str) {
    let _l = lock(id);
    mark_locked(id, ids, how);
}

/// 门铃响了:没送的全取出来,记成送到了。有该叫醒的才取;只有不叫醒的就留着,等下次一起带
pub fn take(id: &str) -> Vec<Msg> {
    let _l = lock(id);
    let p = pending(id);
    if !p.iter().any(|m| m.wake) {
        return vec![];
    }
    mark_locked(id, &p.iter().map(|m| m.id.clone()).collect::<Vec<_>>(), "bell");
    p
}

/// 排进了 Codex 的队列:还不算送到,等它为这几条开一轮(UserPromptSubmit 里看到原话)再算
fn mark_queued(id: &str, ids: &[String]) {
    let _l = lock(id);
    let mut l = ledger(id);
    l.queued.extend(ids.iter().filter(|x| !l.queued.contains(x) && !l.got.contains(x)).cloned().collect::<Vec<_>>());
    save(id, &l);
}

pub fn remove(id: &str) {
    for ext in ["jsonl", "state", "bell", "lock"] {
        let _ = std::fs::remove_file(paths::inbox_dir().join(format!("{id}.{ext}")));
    }
}

fn ago(secs: u64) -> String {
    match secs {
        0..60 => "刚刚".into(),
        60..3600 => format!("{} 分钟前", secs / 60),
        3600..86400 => format!("{} 小时前", secs / 3600),
        _ => format!("{} 天前", secs / 86400),
    }
}

fn what_said(what: &str) -> &'static str {
    match what {
        "turn.done" => "跑完一轮",
        "turn.failed" => "出错了",
        "agent.exited" => "退出了",
        "agent.waiting" => "在等确认",
        _ => "有动静",
    }
}

/// 门铃(Claude)取走的那一段,它当成系统提示看到:几条一起列出来,谁发的、多久前、怎么回。
/// 通知只写发生了什么、去哪看,不写「你去做 X」:写成命令,模型可能当成可疑指令。
/// 派它出去的那个(`parent`)发来的标明是交代的活:不标的话,要它跑命令时 Claude 会当成外人的指令不做(2.1.295 实测)。
/// `hears_done` = 它这一轮跑完 parent 会收到通知:在原地回答就送回去了,不用再 `aca msg`
pub fn render(msgs: &[Msg], now: u64, parent: &str, hears_done: bool) -> String {
    let ids: Vec<&str> = msgs.iter().map(|m| m.id.as_str()).collect();
    let mut out = format!("{} 条新消息({})· 全部:aca inbox", msgs.len(), ids.join(" "));
    for (i, m) in msgs.iter().enumerate() {
        let when = ago(now.saturating_sub(m.ts));
        let who = if m.from == "you" { "用户(在会话外面)".to_string() } else { m.who.clone() };
        if m.kind == "notice" {
            out += &format!("\n{}. {who} · {} · {when}\n   {}", i + 1, what_said(&m.what), m.text.replace('\n', "\n   "));
        } else {
            let back = match m.from.as_str() {
                "you" => String::new(),
                f if f == parent && hears_done => "  派你干活的会话交代的:直接在这里做、在这里回答,这一轮跑完它会收到你最后一句".into(),
                f if f == parent => format!("  派你干活的会话交代的:直接在这里做;要回它用 aca msg {f} \"…\""),
                f => format!("  回话:aca msg {f} \"…\""),
            };
            out += &format!("\n{}. {who} · 发来一句话 · {when}\n   「{}」{back}", i + 1, m.text.replace('\n', "\n   "));
        }
    }
    out
}

/// 排队 / 打字送进去的那段:在它看来就是一条用户消息,所以只放话本身。你发的原样送;会话发的前面一行写是谁;
/// 通知写发生了什么。不写「这是别的会话转来的、不是用户打的字」,也不教它怎么回:Codex 看到这些会当成注入、
/// 不照做;只写一行「来自哪个会话」它照做(0.162 实测)。`parent` = 派收信这个会话出来的那个
fn render_as_input(msgs: &[Msg], now: u64, parent: &str) -> String {
    let one = |m: &Msg| match (m.kind.as_str(), m.from.as_str()) {
        ("notice", _) => format!("[aca] {} {} · {}\n{}", m.who, what_said(&m.what), ago(now.saturating_sub(m.ts)), m.text),
        (_, "you") => m.text.clone(),
        (_, f) if f == parent => format!("[aca] 派你干活的会话 {} 说:\n{}", m.who, m.text),
        _ => format!("[aca] 来自会话 {}:\n{}", m.who, m.text),
    };
    msgs.iter().map(one).collect::<Vec<_>>().join("\n\n")
}

/// 它这一轮跑完,派它出去的那个会不会收到通知(见 [`render`])
fn hears_done(cfg: &Config, s: &Session) -> bool {
    !s.parent.is_empty() && wanted(&runtime::load(&s.id).notify, &cfg.inbox.notify, "done")
}

/// 排进了队列、这段用户消息里带着原话的那几条:Codex 为它们开了一轮
fn queued_in(id: &str, prompt: &str) -> Vec<String> {
    let queued = ledger(id).queued;
    if queued.is_empty() {
        return vec![];
    }
    all(id)
        .into_iter()
        .filter(|m| queued.contains(&m.id) && !m.text.trim().is_empty() && prompt.contains(m.text.trim()))
        .map(|m| m.id)
        .collect()
}

/// 输入框里有没有你没发的字:找最后一行以 `mark` 开头的(那是输入框),看记号后面有没有不是灰色(dim)的字。
/// 两家的提示(`Try "…"`、`Ask Codex to do anything`)都是灰的(2026-10-08 实测)。None = 屏幕上没找到输入框
pub fn draft(mark: char, lines: &[String]) -> Option<bool> {
    let line = lines.iter().rev().find(|l| plain(l).trim_start().starts_with(mark))?;
    let mut dim = false;
    let mut seen_mark = false;
    let mut cs = line.chars().peekable();
    while let Some(c) = cs.next() {
        if c == '\x1b' {
            if cs.peek() == Some(&'[') {
                cs.next();
                let mut seq = String::new();
                for d in cs.by_ref() {
                    if d.is_ascii_alphabetic() {
                        if d == 'm' {
                            dim = sgr_dim(&seq, dim);
                        }
                        break;
                    }
                    seq.push(d);
                }
            }
            continue;
        }
        if !seen_mark {
            seen_mark = c == mark;
            continue;
        }
        if !c.is_whitespace() && !dim {
            return Some(true);
        }
    }
    Some(false)
}

/// 去掉颜色
fn plain(line: &str) -> String {
    let mut out = String::new();
    let mut cs = line.chars().peekable();
    while let Some(c) = cs.next() {
        if c == '\x1b' && cs.peek() == Some(&'[') {
            cs.next();
            for d in cs.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// 一段 SGR(`ESC[…m` 中间那串)之后还是不是灰的:2 是灰,0 / 22 / 空 是取消;38、48 后面跟着的是颜色,跳过
fn sgr_dim(seq: &str, mut dim: bool) -> bool {
    let ps: Vec<&str> = seq.split(';').collect();
    let mut i = 0;
    while i < ps.len() {
        match ps[i] {
            "" | "0" | "22" => dim = false,
            "2" => dim = true,
            "38" | "48" | "58" => i += if ps.get(i + 1) == Some(&"5") { 2 } else { 4 },
            _ => {}
        }
        i += 1;
    }
    dim
}

/// 发的那个给人看的样子:`ab12「修登录」claude:sub`
pub fn who(s: &Session, r: &Run) -> String {
    let acct = if s.account.is_empty() { s.tool.clone() } else { format!("{}:{}", s.tool, agent::account_label(&s.account)) };
    format!("{}「{}」{acct}", s.id, status::title(s, r))
}

/// 这件事要不要通知 parent:会话自己定了的(`aca new --notify`)听它的,没定的听配置的,配置也没写就都不通知
/// (不开口就不叫醒谁)。`all` = 四样都要
fn wanted(own: &str, default: &[String], short: &str) -> bool {
    let own: Vec<&str> = own.split(',').map(str::trim).filter(|x| !x.is_empty()).collect();
    let list: Vec<&str> = if own.is_empty() { default.iter().map(String::as_str).collect() } else { own };
    list.iter().any(|x| *x == short || *x == "all")
}

/// 通知里那一段:只写发生了什么、去哪看
fn notice_text(id: &str, ev: &str, r: &Run) -> String {
    let one_line = |s: &str, n: usize| clip(&s.split_whitespace().collect::<Vec<_>>().join(" "), n);
    match ev {
        "turn.done" if r.last.trim().is_empty() => format!("全文:aca output {id}"),
        "turn.done" => format!("最后一句:「{}」  全文:aca output {id}", one_line(&r.last, 120)),
        "turn.failed" if r.error_kind.is_empty() => format!("原因:{}", one_line(&r.error, 200)),
        "turn.failed" => format!("原因:{}({})", one_line(&r.error, 200), r.error_kind_said()),
        "agent.exited" => "它的进程退出了".into(),
        _ => match r.ask_kind.as_str() {
            "choice" if r.ask_multi => format!("在问:「{}」(一次问了好几题,得在它的界面里答)", r.ask),
            "choice" => {
                let opts: Vec<String> = r.ask_opts.iter().enumerate().map(|(i, o)| format!("{} {o}", i + 1)).collect();
                format!("在问:「{}」\n选项:{}  回答:aca answer {id} <选项号>", r.ask, opts.join("  "))
            }
            "permission" => format!("要你同意:{}\n回答:aca answer {id} yes|no", r.ask),
            _ => format!("在等你:{}", r.ask),
        },
    }
}

/// worker 这一头,在它自己的 hook 里:这件事要不要告诉派它的那个,要的话放一条进那边的收件箱,
/// 再起一个不等的送信进程。返回放进去的那条
pub fn notify(id: &str, ev: &str, cfg: &Config) -> Option<Msg> {
    let short = match ev {
        "turn.done" => "done",
        "turn.failed" => "failed",
        "agent.exited" => "exited",
        "agent.waiting" => "waiting",
        _ => return None,
    };
    let r = runtime::load(id);
    if !wanted(&r.notify, &cfg.inbox.notify, short) {
        return None;
    }
    let store = Store::open().ok()?;
    let s = store.get(id).ok()?;
    let parent = store.get(&s.parent).ok().filter(|p| p.id != s.id)?;
    let m = post(Msg {
        from: s.id.clone(),
        who: who(&s, &r),
        to: parent.id.clone(),
        kind: "notice".into(),
        what: ev.into(),
        wake: true,
        text: notice_text(&s.id, ev, &r),
        ..Default::default()
    });
    later(&parent.id, 0, &[]);
    Some(m)
}

/// 收信这一头,在它自己的 hook 里(不等任何东西):
/// - 这一轮的那段话里有排进队列的那几条原话:记成送到了
/// - 这一轮跑完了、刚起来:还有没送的,起一个不等的送信进程(门铃那一家刚起来时由 `aca inbox watch` 管)
pub fn on_hook(id: &str, ev: &str, prompt: &str, bell_agent: bool) {
    if ev == "UserPromptSubmit" {
        let ids = queued_in(id, prompt);
        if !ids.is_empty() {
            mark(id, &ids, "queue");
        }
    }
    let again = matches!(ev, "Stop" | "StopFailure") || (ev == "SessionStart" && !bell_agent);
    if again && wants_wake(id) {
        later(id, 1, &[]);
    }
}

/// `aca start` / `new` 刚把它起起来:有没送的,过几秒送一次。门铃那一家等它自己接上门铃时送(`aca inbox watch`);
/// 别的不能等它的 SessionStart —— Codex 的要等第一句话才报
pub fn launched(cfg: &Config, s: &Session) {
    let bell = agent::of(cfg, s).is_ok_and(|(a, _)| a.delivery() == Delivery::Bell);
    if !bell && wants_wake(&s.id) {
        later(&s.id, 5, &[]);
    }
}

/// 在跑 / 在等确认先不送,一般等它自己这一轮跑完的 hook。屏幕上像在跑、hook 却没说有一轮在跑的
/// (刚起来在加载、启动时的几道关),之后没有那个 hook:过一阵自己再看一次
fn wait_turn(id: &str, retry: bool, why: &str) -> Res<Sent> {
    if !retry && !matches!(runtime::load(id).status.as_str(), "running" | "waiting") {
        later(id, RETRY_SECS, &["--retry"]);
    }
    Ok(Sent::Held(why.into()))
}

/// 过 `secs` 秒在后台送一次(`aca inbox deliver`):不等它,hook 和命令马上返回
fn later(id: &str, secs: u64, more: &[&str]) {
    let _ = Command::new(agent::self_exe())
        .args(["inbox", "deliver", id, "--after", &secs.to_string()])
        .args(more)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
}

fn ring(id: &str) {
    let _ = std::fs::create_dir_all(paths::inbox_dir());
    // 原地改,不换文件:换了文件,盯着它的那一头可能就跟丢了
    let _ = std::fs::write(bell(id), paths::now_ms().to_string());
}

/// 发的那一刻(和之后每次补送)送一次:看对方是哪一家、现在在干什么,挑一种送法。`retry` = 这已经是隔了一阵的再试
pub fn deliver(store: &Store, cfg: &Config, id: &str, retry: bool) -> Res<Sent> {
    let s = store.get(id)?;
    let msgs = pending(&s.id);
    if !msgs.iter().any(|m| m.wake) {
        return Ok(Sent::Nothing);
    }
    let (agent, _) = agent::of(cfg, &s)?;
    let r = runtime::load(&s.id);
    let pane = tmux::find(&s.id).filter(|p| !p.dead);
    let held = |why: &str| Ok(Sent::Held(why.to_string()));
    match agent.delivery() {
        Delivery::Keep => held("它是个 shell,不往里打字;在 aca inbox 里看"),
        Delivery::Bell if pane.is_none() => held("它没在跑:起来时再送"),
        Delivery::Bell if r.bell => {
            ring(&s.id);
            let ids: Vec<String> = msgs.iter().filter(|m| m.wake).map(|m| m.id.clone()).collect();
            later(&s.id, BELL_CHECK_SECS, &["--check", &ids.join(",")]);
            Ok(Sent::Bell)
        }
        Delivery::Queue if !r.conv.is_empty() => {
            if pane.is_some() {
                match status::one(cfg, &s, true).state {
                    State::Running => return wait_turn(&s.id, retry, "它在跑:这一轮跑完再排进去"),
                    State::Ask => return wait_turn(&s.id, retry, "它在等确认:确认完、这一轮跑完再排进去"),
                    // 被打断的对话,Codex 自己也不开跑排着的(退出再 resume 起来也一样,0.162 实测),等下一轮正常跑完
                    _ if r.event == "Interrupt" => return held("你刚按 Esc 打断了它:等你下一轮跑完再送"),
                    _ => {}
                }
            }
            let ids: Vec<String> = msgs.iter().map(|m| m.id.clone()).collect();
            match enqueue(cfg, &s, &*agent, &r.conv, &render_as_input(&msgs, paths::now(), &s.parent)) {
                Ok(()) => {
                    mark_queued(&s.id, &ids);
                    Ok(Sent::Queued)
                }
                Err(e) if pane.is_none() => held(&format!("排不进它的队列({e}),它也没在跑:起来后再送")),
                Err(_) => type_in(cfg, &s, &*agent, &pane.unwrap(), &msgs, retry),
            }
        }
        _ => match &pane {
            None => held("它没在跑:起来时再送"),
            Some(p) => type_in(cfg, &s, &*agent, p, &msgs, retry),
        },
    }
}

/// 排进 Codex 的队列:先带 `-c` 在自己进程里写(不拉起后台服务);这个账号已经有后台服务在跑时它会拒绝,就交给那个服务
fn enqueue(cfg: &Config, s: &Session, agent: &dyn Agent, conv: &str, text: &str) -> Res<()> {
    let run = |shared: bool| -> Res<()> {
        let l = agent::tool_cmd(cfg, s, &agent.queue_args(conv, text, shared))?;
        let mut c = Command::new(&l.argv[0]);
        c.args(&l.argv[1..]).envs(l.env).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        // 包了一层脚本的(按窗格改配色):别让它以为是在哪一格里起的
        c.env_remove("TMUX").env_remove("TMUX_PANE");
        if std::path::Path::new(&s.dir).is_dir() {
            c.current_dir(&s.dir);
        }
        let mut child = c.spawn().map_err(|e| format!("起不来:{e}"))?;
        let t = Instant::now();
        let status = loop {
            if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
                break st;
            }
            if t.elapsed() > Duration::from_secs(20) {
                let _ = child.kill();
                return Err("20 秒没回".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        if status.success() { Ok(()) } else { Err(clip(err.trim(), 300)) }
    };
    match run(false) {
        Err(e) if e.contains("daemon is running") => run(true),
        other => other,
    }
}

/// 打字送进去:它空着、你没在那一格打字、输入框里没有你没发的字才打;不然过一阵再看一次
fn type_in(cfg: &Config, s: &Session, agent: &dyn Agent, pane: &tmux::Pane, msgs: &[Msg], retry: bool) -> Res<Sent> {
    let why = match status::one(cfg, s, true).state {
        State::Running => return wait_turn(&s.id, retry, "它在跑:这一轮跑完再送"),
        State::Ask => return wait_turn(&s.id, retry, "它在等确认:不往确认框里打字"),
        _ if tmux::typing(&pane.pane, TYPING_SECS) => Some("你正在那一格打字"),
        _ => match agent.prompt_mark().map(|m| draft(m, &tmux::capture_ansi(&pane.pane))) {
            Some(Some(true)) => Some("输入框里有你没发的字"),
            Some(None) => Some("屏幕上没看到它的输入框"),
            _ => None,
        },
    };
    if let Some(why) = why {
        // 你在打字、有草稿的:过一阵再看一次;还不行就等它下次的 hook
        if !retry {
            later(&s.id, RETRY_SECS, &["--retry"]);
        }
        return Ok(Sent::Held(why.into()));
    }
    tmux::paste(&pane.pane, &render_as_input(msgs, paths::now(), &s.parent))?;
    std::thread::sleep(Duration::from_millis(150));
    tmux::keys(&pane.pane, &["Enter"])?;
    mark(&s.id, &msgs.iter().map(|m| m.id.clone()).collect::<Vec<_>>(), "type");
    Ok(Sent::Typed)
}

/// `aca msg`:放一条进对方的收件箱,马上送一次。在会话里调的,发件人是那个会话;`parent` = 派它的那个
pub fn send(store: &Store, cfg: &Config, to: &str, text: &str) -> Res<(Msg, Sent)> {
    let me = std::env::var("ACA_SESSION_ID").ok().filter(|x| !x.is_empty()).and_then(|x| store.get(&x).ok());
    let to = match to {
        "parent" => {
            let me = me.as_ref().ok_or("parent 是派你的那个:要在某个会话里发")?;
            store.get(&me.parent).map_err(|_| "你没有 parent(不是被派出来的)".to_string())?
        }
        key => store.get(key)?,
    };
    let (from, who) = match &me {
        Some(m) if m.id == to.id => return Err("不能发给自己".into()),
        Some(m) => (m.id.clone(), who(m, &runtime::load(&m.id))),
        None => ("you".to_string(), "你".to_string()),
    };
    let m = post(Msg { from, who, to: to.id.clone(), kind: "msg".into(), wake: true, text: clip(text.trim(), 4000), ..Default::default() });
    if !m.wake {
        return Ok((m, Sent::Held("10 分钟里发得太多了:这条只存不叫醒".into())));
    }
    let sent = deliver(store, cfg, &to.id, false)?;
    Ok((m, sent))
}

/// `aca inbox take`:Claude 的门铃 hook(FileChanged,asyncRewake)。有该叫醒的就写到 stderr、退出码 2;没有退出 0
pub fn take_cmd() -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let Some(id) = std::env::var("ACA_SESSION_ID").ok().filter(|s| !s.is_empty()) else { return 0 };
    // Claude 的后台会话继承了原窗格的环境,不是这个会话本人
    if std::env::var_os("CLAUDE_JOB_DIR").is_some() {
        return 0;
    }
    let got = take(&id);
    if got.is_empty() {
        return 0;
    }
    let me = Store::open().ok().and_then(|st| st.get(&id).ok()).unwrap_or_default();
    eprint!("{}", render(&got, paths::now(), &me.parent, hears_done(&crate::config::load(), &me)));
    2
}

/// `aca inbox watch`:Claude 的 SessionStart 多跑的那一条。回一个 `watchPaths` 让它盯着门铃;有没送的,过两秒(它盯上之后)按一次
pub fn watch_cmd() -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let Some(id) = std::env::var("ACA_SESSION_ID").ok().filter(|s| !s.is_empty()) else { return 0 };
    if std::env::var_os("CLAUDE_JOB_DIR").is_some() {
        return 0;
    }
    let b = bell(&id);
    if !b.exists() {
        ring(&id);
    }
    runtime::update(&id, |r| r.bell = true);
    println!("{}", json!({ "hookSpecificOutput": { "hookEventName": "SessionStart", "watchPaths": [b] } }));
    if wants_wake(&id) {
        later(&id, 2, &[]);
    }
    0
}

/// `aca inbox deliver <id> [--after 秒] [--retry] [--check 编号,…]`:hook 和命令留的后手,过一阵送一次。
/// `--check`:门铃按过了,这几条还没被取走 = 这次起来的 Claude 没接上门铃,以后改用打字
pub fn deliver_cmd(id: &str, after: u64, retry: bool, check: &[String]) -> i32 {
    std::thread::sleep(Duration::from_secs(after));
    // 等的这一阵会话删了:不去开登记表(开了会把整个目录重新建出来)
    if !paths::run_file(id).exists() {
        return 0;
    }
    if !check.is_empty() {
        if !pending(id).iter().any(|m| check.contains(&m.id)) {
            return 0;
        }
        runtime::update(id, |r| r.bell = false);
    }
    let Ok(store) = Store::open() else { return 0 };
    let _ = deliver(&store, &crate::config::load(), id, retry);
    0
}

/// `aca wait` 等到的结果
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    /// 跑完一轮(或者空下来了)
    Done,
    Asking,
    Failed,
    Exited,
    Timeout,
}

impl Outcome {
    pub fn code(self) -> i32 {
        match self {
            Outcome::Done => 0,
            Outcome::Asking => 2,
            Outcome::Failed => 3,
            Outcome::Exited => 4,
            Outcome::Timeout => 124,
        }
    }
}

/// 一个会话现在算不算「等到了」。`since` = 开始等的时刻:刚开始那几秒,跑完的得是开始等之后跑完的
/// (刚发进去的话还没变成「在跑」,它看起来还是上一轮的「跑完了」);手上还有没送到的消息的,算还没开始
fn outcome(cfg: &Config, s: &Session, since: u64, now: u64) -> Option<Outcome> {
    let r = runtime::load(&s.id);
    // 读屏幕:Claude 被 Esc 打断不发 hook,只看 hook 的话它一直「在跑」
    let state = status::one(cfg, s, true).state;
    match state {
        State::Running => None,
        // 启动时问「信不信任这个目录」、开关开着:起它时留的后手正要替你答,还没到要你管的时候
        State::Ask if crate::trust::answering(cfg, s) => None,
        State::Ask => Some(Outcome::Asking),
        State::Error => Some(Outcome::Failed),
        State::Exited | State::Stopped => Some(Outcome::Exited),
        State::Idle | State::Unread => {
            let busy = list(&s.id).iter().any(|(m, st)| m.wake && *st != Stage::Got);
            let never = r.turn == 0 && r.done == 0;
            let early = now.saturating_sub(since) < 3 && r.done < since;
            (!busy && !never && !early).then_some(Outcome::Done)
        }
    }
}

/// `aca wait`:等这几个会话跑完一轮 / 在等确认 / 出错 / 退出。`any` = 等到一个就回。
/// 在某个会话里调的:等到的那几个发来的通知记成送到了,门铃就不会再为它们叫醒一次
pub fn wait(store: &Store, cfg: &Config, keys: &[String], any: bool, timeout: Option<Duration>) -> Res<Vec<(Session, Outcome)>> {
    let sessions: Vec<Session> = keys.iter().map(|k| store.get(k)).collect::<Res<_>>()?;
    if sessions.is_empty() {
        return Err("要给会话".into());
    }
    let since = paths::now();
    let t = Instant::now();
    let mut done: Vec<Option<Outcome>> = vec![None; sessions.len()];
    loop {
        let now = paths::now();
        for (s, d) in sessions.iter().zip(done.iter_mut()) {
            if d.is_none() {
                *d = outcome(cfg, s, since, now);
            }
        }
        let finished = done.iter().filter(|d| d.is_some()).count();
        if finished == sessions.len() || (any && finished > 0) {
            break;
        }
        if timeout.is_some_and(|w| t.elapsed() >= w) {
            if !any {
                done.iter_mut().filter(|d| d.is_none()).for_each(|d| *d = Some(Outcome::Timeout));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let out: Vec<(Session, Outcome)> = sessions.into_iter().zip(done).filter_map(|(s, d)| d.map(|d| (s, d))).collect();
    if let Some(me) = std::env::var("ACA_SESSION_ID").ok().filter(|x| !x.is_empty()) {
        // 那几个的 hook 正在往这边放通知:等它落下来,再记成送到了
        std::thread::sleep(Duration::from_millis(600));
        let from: Vec<&str> = out.iter().filter(|(_, d)| *d != Outcome::Timeout).map(|(s, _)| s.id.as_str()).collect();
        let ids: Vec<String> = pending(&me)
            .into_iter()
            .filter(|m| m.kind == "notice" && from.contains(&m.from.as_str()) && m.ts + 2 >= since)
            .map(|m| m.id)
            .collect();
        if !ids.is_empty() {
            mark(&me, &ids, "wait");
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, kind: &str, text: &str) -> Msg {
        Msg {
            id: id.into(),
            ts: 100,
            from: "ef56".into(),
            who: "ef56「读文档」codex:labs".into(),
            to: "ab12".into(),
            kind: kind.into(),
            what: if kind == "notice" { "turn.done".into() } else { String::new() },
            wake: true,
            text: text.into(),
        }
    }

    #[test]
    fn what_the_agent_sees_says_who_what_and_where_to_look_and_carries_the_ids() {
        let a = msg("m0000000a", "notice", "最后一句:「整理好了」  全文:aca output ef56");
        let b = Msg { from: "you".into(), ..msg("m0000000b", "msg", "先别动数据库\n等我回来") };
        let c = msg("m0000000c", "msg", "补一下测试");
        let text = render(&[a.clone(), b.clone(), c.clone()], 220, "", false);
        assert!(text.starts_with("3 条新消息(m0000000a m0000000b m0000000c)"), "{text}");
        assert!(text.contains("1. ef56「读文档」codex:labs · 跑完一轮 · 2 分钟前\n   最后一句:「整理好了」"), "{text}");
        // 多行的话缩进对齐;用户在外面发的没有「回话」
        assert!(text.contains("2. 用户(在会话外面) · 发来一句话 · 2 分钟前\n   「先别动数据库\n   等我回来」\n"), "{text}");
        assert!(text.contains("「补一下测试」  回话:aca msg ef56"), "{text}");
        // 派它干活的那个发来的:标明是交代的活;它听「跑完」的,原地回答就送回去了,不教它再 aca msg 一遍
        let text = render(std::slice::from_ref(&c), 220, "ef56", true);
        assert!(text.contains("「补一下测试」  派你干活的会话交代的:直接在这里做、在这里回答") && !text.contains("aca msg"), "{text}");
        let text = render(std::slice::from_ref(&c), 220, "ef56", false);
        assert!(text.contains("「补一下测试」  派你干活的会话交代的:直接在这里做;要回它用 aca msg ef56"), "{text}");
        // 排队 / 打字进去的:用户发的原样,会话发的只多一行是谁,不说「不是用户打的」、不教怎么回
        let other = Msg { from: "zz99".into(), who: "zz99「别的」claude:sub".into(), ..c.clone() };
        let input = render_as_input(&[a, b, c, other], 220, "ef56");
        assert_eq!(
            input,
            "[aca] ef56「读文档」codex:labs 跑完一轮 · 2 分钟前\n最后一句:「整理好了」  全文:aca output ef56\n\n\
             先别动数据库\n等我回来\n\n\
             [aca] 派你干活的会话 ef56「读文档」codex:labs 说:\n补一下测试\n\n\
             [aca] 来自会话 zz99「别的」claude:sub:\n补一下测试"
        );
    }

    #[test]
    fn a_draft_is_any_non_dim_character_after_the_prompt_mark() {
        let l = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // 两家的提示都是灰的(SGR 2),2026-10-08 抓的样子
        assert_eq!(draft('❯', &l(&["● 好了", "\x1b[0m❯ \x1b[2mTry \"fix typecheck errors\"\x1b[0m"])), Some(false));
        assert_eq!(draft('›', &l(&["\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[22m"])), Some(false));
        assert_eq!(draft('❯', &l(&["❯ "])), Some(false));
        // 你打了字
        assert_eq!(draft('❯', &l(&["❯ 人打的草稿"])), Some(true));
        assert_eq!(draft('›', &l(&["\x1b[2m›\x1b[0m half"])), Some(true));
        // 灰的后面接着正常颜色的字(取消了灰)
        assert_eq!(draft('❯', &l(&["❯ \x1b[2mTry\x1b[22m x"])), Some(true));
        // 颜色参数里的 2 不是灰
        assert_eq!(draft('❯', &l(&["❯ \x1b[38;2;2;2;2mx"])), Some(true));
        // 对话里早先的那几行也以记号开头:看最后一个
        assert_eq!(draft('›', &l(&["› 上一句我说的", "• 回复", "› \x1b[2mAsk Codex to do anything"])), Some(false));
        // 没有输入框(在等你选、界面还没画出来)
        assert_eq!(draft('❯', &l(&["Loading…"])), None);
    }

    #[test]
    fn who_gets_told_follows_the_session_then_the_config_and_otherwise_nobody() {
        // 谁都没说要:不通知
        assert!(!wanted("", &[], "done") && !wanted("", &[], "waiting"));
        let cfg = vec!["done".to_string(), "failed".to_string()];
        assert!(wanted("", &cfg, "done") && !wanted("", &cfg, "waiting"));
        assert!(wanted("waiting, exited", &cfg, "waiting") && !wanted("waiting,exited", &cfg, "done"));
        assert!(!wanted("none", &cfg, "done"));
        assert!(wanted("all", &[], "exited") && wanted("", &["all".to_string()], "waiting"));
    }

    #[test]
    fn a_notice_only_says_what_happened_and_how_to_answer() {
        let r = Run { last: "改好了。\n\n细节:\n  1. 改了 a.rs".into(), ..Default::default() };
        assert_eq!(notice_text("w2", "turn.done", &r), "最后一句:「改好了。 细节: 1. 改了 a.rs」  全文:aca output w2");
        let ask =
            Run { ask: "用哪个库?".into(), ask_kind: "choice".into(), ask_opts: vec!["A".into(), "B".into()], ..Default::default() };
        assert_eq!(notice_text("w2", "agent.waiting", &ask), "在问:「用哪个库?」\n选项:1 A  2 B  回答:aca answer w2 <选项号>");
        let perm = Run { ask: "Bash:rm -rf build".into(), ask_kind: "permission".into(), ..Default::default() };
        assert!(notice_text("w2", "agent.waiting", &perm).ends_with("aca answer w2 yes|no"));
        let multi = Run { ask_multi: true, ..ask };
        assert!(notice_text("w2", "agent.waiting", &multi).contains("界面里答"));
    }

    #[test]
    fn sgr_turns_dim_on_and_off() {
        assert!(sgr_dim("2", false) && sgr_dim("1;2", false) && !sgr_dim("0", true) && !sgr_dim("22", true) && !sgr_dim("", true));
        assert!(!sgr_dim("38;5;2", false) && sgr_dim("38;5;8;2", false));
        assert_eq!(plain("\x1b[1m›\x1b[0m hi"), "› hi");
    }
}
