//! 列表里的子代理:Claude Code / Codex 自己派出去的(Claude 的 Agent 工具、Codex 的 spawn_agent)。
//!
//! 它们跑在父会话的进程里,没有自己的窗格,只有记录文件。所以这里读的就是 `Z` 那棵树(`trace`),
//! 「在不在跑」和 `Z` 页是同一份记录、同一个判断;只读记录,不加 hook。
//! - 哪些会话去读:Claude 看记录旁边的 `subagents/` 里有没有最近动过的,Codex 看 rollout 里起没起过子线程。
//!   没有的不解析(一份记录能有几十 MB);有的读一次,之后每次刷新只读新增的那截。
//! - 列哪些:还在跑的(哪一轮起的都算),加上最后一轮里起的(跑完了也留着);更早的在 `Z` 里看。
//! - 主对话停了、后台的子代理还在跑:这个会话算在跑,不算「跑完没看」—— 不然进去一看,它还在等子代理。
//!
//! 列表里怎么画、按键怎么走在 ui.rs(`l` 展开成行,`⏎` 进 `Z` 页里它自己的记录)。

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use aca_core::runtime::Run;
use aca_core::t;
use memchr::memmem;
use ratatui::style::{Color, Modifier, Style};

use crate::data::{Agent, Session, State};
use crate::theme;
use crate::trace::{Kind, St, Trace, clock, turn_of};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Running,
    Done,
    Failed,
    /// 被打断 / 被停掉,或者会话关掉时它没写完
    Stopped,
}

/// 一个子代理,列表里一行要的东西
#[derive(Clone, Debug, PartialEq)]
pub struct Subagent {
    /// Claude 的 agentId / Codex 的线程 id
    pub id: String,
    /// Claude:类型(Explore、general-purpose)。Codex:它的名字(Cicero —— Codex 自己的界面里也这么叫它)
    pub kind: String,
    /// 派给它的事。Claude:description。Codex:任务名(0.144+ 的 task_name),老版本是「类型 · 派活那句话的头一行」
    pub desc: String,
    pub phase: Phase,
    /// 起的时刻(毫秒)
    pub t0: i64,
    /// 从头到尾多久(毫秒);在跑的算到采集那一刻
    pub dur: i64,
    /// 用了几个工具
    pub tools: usize,
    /// 是别的子代理起的:隔了几层(0 = 会话自己起的)
    pub depth: u8,
    /// 预览里画的几行(带颜色,和会话的屏幕一个画法)。还在跑:派给它的话、一步一行,最新的在最下面,预览照屏幕那样露底下那截。
    /// 交回话了(`reported`):它交回来的话排最前,后面才是派给它的话和那几步,预览从头露 —— 要看的是结论的开头
    pub screen: Vec<String>,
    pub reported: bool,
}

impl Subagent {
    /// 「类型 · 说明」。general-purpose 是没指定类型时的那种,不写:一排十几个时它把真正分得清的说明挤没了
    pub fn label(&self) -> String {
        match (self.kind.as_str(), self.desc.as_str()) {
            ("" | "general-purpose", d) if !d.is_empty() => d.to_string(),
            ("", d) => d.to_string(),
            (k, "") => k.to_string(),
            (k, d) => format!("{k} · {d}"),
        }
    }

    pub fn running(&self) -> bool {
        self.phase == Phase::Running
    }
}

/// 这么久(秒)没动过的子代理记录不再看:会话空着时,跑完的子代理最多再留这么久
const QUIET: u64 = 30 * 60;
/// 预览里最多列这么多步、派给它的话 / 交回来的话各最多这么多行
const STEPS: usize = 40;
const TASK_LINES: usize = 6;
const REPLY_LINES: usize = 30;

/// 一份记录里要列的子代理,按起的先后,它再起的紧跟在它后面。`busy` = 会话这一轮还在跑(没回结果的 Agent 工具才算数),
/// `alive` = agent 进程还在(后台跑的子代理靠它活着)
pub(crate) fn list(t: &Trace, busy: bool, alive: bool, now: i64) -> Vec<Subagent> {
    let mut out = vec![];
    walk(t, busy, alive, now, None, 0, &mut out);
    out
}

/// 一段对话里起过的所有子代理,不管哪一轮起的;当成它最后一次有动静之后一秒、进程还在。拿本机真的记录看的那几个测试用
#[cfg(test)]
pub(crate) fn every(t: &Trace) -> (Vec<Subagent>, i64) {
    fn last(t: &Trace) -> i64 {
        t.subs.iter().map(|s| last(&s.t)).fold(t.last_active(), i64::max)
    }
    let now = last(t) + 1000;
    let mut out = vec![];
    walk(t, false, true, now, Some(true), 0, &mut out);
    (out, now)
}

/// `recent`:上面那一层已经定了算不算最后一轮里的(嵌套的跟着起它的那个走)
fn walk(t: &Trace, busy: bool, alive: bool, now: i64, recent: Option<bool>, depth: u8, out: &mut Vec<Subagent>) {
    let last = t.last_turn();
    for i in 0..t.nodes.len() {
        let Some(s) = t.sub_of(i) else { continue };
        let run = running(t, i, busy, alive, now);
        let rec = recent.unwrap_or_else(|| Some(turn_of(t, i)) == last);
        if run || rec {
            let phase = match (run, t.sub_state(i)) {
                (true, _) => Phase::Running,
                (_, St::Err) => Phase::Failed,
                (_, St::Stopped | St::Denied) => Phase::Stopped,
                (_, St::Ok) if !s.t.finished() => Phase::Stopped,
                (_, St::Ok) => Phase::Done,
            };
            let first = s.t.roots.iter().map(|&r| s.t.nodes[r].t0).find(|&x| x > 0).unwrap_or(t.nodes[i].t0);
            let (kind, desc) = named(t, i, s);
            let (screen, reported) = screen(&s.t, phase, &desc);
            out.push(Subagent {
                id: s.id.clone(),
                screen,
                reported,
                kind,
                desc,
                phase,
                t0: first,
                dur: t.sub_dur(i, run, now),
                tools: s.t.nodes.iter().filter(|n| n.kind == Kind::Tool).count(),
                depth,
            });
        }
        // 它再起的:它自己得还在跑,里面没回结果的才算在跑
        walk(&s.t, run, alive && run, now, Some(rec), depth.saturating_add(1), out);
    }
}

/// 一行上写的两段。Claude 的记录里现成(类型、说明)。Codex 的子线程自己只记着名字和类型(`Sub` 的 desc / kind),
/// 派给它的事要另找:它的 `agent_path` 最后一段,没有(老版本)就用起它的那个 spawn_agent 的摘要,再没有就只剩类型
fn named(t: &Trace, tool: usize, s: &crate::trace::Sub) -> (String, String) {
    if t.agent != Agent::Codex {
        return (s.kind.clone(), s.desc.clone());
    }
    let n = &t.nodes[tool];
    let task = match crate::trace_codex::task_of(&s.t) {
        "" if n.name == "Agent" && !n.label.is_empty() => n.label.clone(),
        "" => s.kind.clone(),
        x => x.to_string(),
    };
    (s.desc.clone(), task)
}

/// 起它的那个工具 i 的子代理还在不在跑。
/// 工具还没回结果(前台跑的):它那一轮得还在跑 —— 按 Esc 打断之后子代理的记录停在半截,不能再算在跑。
/// 工具早就回了(后台跑的、Codex 的子线程):看它自己写没写完(`Trace::sub_live`)
fn running(t: &Trace, i: usize, busy: bool, alive: bool, now: i64) -> bool {
    let n = &t.nodes[i];
    if n.t1 == 0 { busy && t.running(i, true) && t.sub_live(i, true, now) } else { n.state == St::Ok && t.sub_live(i, alive, now) }
}

/// 样式 → SGR 开头(预览照屏幕那样解析,见 `ansi::parse_lines`)
fn paint(text: &str, st: Style) -> String {
    let mut codes: Vec<String> = vec![];
    if st.add_modifier.contains(Modifier::BOLD) {
        codes.push("1".into());
    }
    match st.fg {
        Some(Color::Rgb(r, g, b)) => codes.push(format!("38;2;{r};{g};{b}")),
        Some(Color::Indexed(n)) => codes.push(format!("38;5;{n}")),
        Some(c) => {
            const NAMED: [Color; 16] = [
                Color::Black,
                Color::Red,
                Color::Green,
                Color::Yellow,
                Color::Blue,
                Color::Magenta,
                Color::Cyan,
                Color::Gray,
                Color::DarkGray,
                Color::LightRed,
                Color::LightGreen,
                Color::LightYellow,
                Color::LightBlue,
                Color::LightMagenta,
                Color::LightCyan,
                Color::White,
            ];
            if let Some(k) = NAMED.iter().position(|&x| x == c) {
                codes.push(format!("38;5;{k}"));
            }
        }
        None => {}
    }
    if codes.is_empty() { text.to_string() } else { format!("\x1b[{}m{text}\x1b[0m", codes.join(";")) }
}

/// 前 max 行不空的;多出来的写一行「… 另 N 行」
fn head_lines(text: &str, max: usize, out: &mut Vec<String>) {
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    out.extend(lines.iter().take(max).map(|l| format!("  {l}")));
    if lines.len() > max {
        let more = lines.len() - max;
        out.push(paint(&aca_core::tr!("  … 还有 {more} 行", "  … {more} more lines"), crate::ui::dim()));
    }
}

/// 子代理没有屏幕:拿它的记录写几行当屏幕 —— 派给它的话、一步一行(最新的在最下面)。交回话了的把那段话排到最前面
/// (返回的 bool;见 `Subagent::screen`)。派给它的话记录里读不出来时(Codex 0.144+ 线程之间递的话是加密的)写 `task`(任务名)
fn screen(t: &Trace, phase: Phase, task: &str) -> (Vec<String>, bool) {
    let p = theme::pal();
    let dim = crate::ui::dim();
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let mut head = vec![];
    if phase != Phase::Running {
        let reply = t
            .last_turn()
            .and_then(|r| crate::trace_page::last_said(t, r))
            .map(|c| crate::trace::said(t, c).1.join("\n\n"))
            .unwrap_or_default();
        if !reply.trim().is_empty() {
            head.push(paint(t!("返回的结果", "Its report"), bold));
            head_lines(&reply, REPLY_LINES, &mut head);
            head.push(String::new());
        }
    }
    let reported = !head.is_empty();
    let mut out = head;
    if let Some(&turn) = t.roots.iter().find(|&&r| t.nodes[r].kind == Kind::Turn) {
        // 谁也不是(空的称呼)= 这一轮没有读得出来的提问
        let (who, said) = crate::trace::asked(t, turn);
        let said = if who.is_empty() { task } else { said.as_str() };
        if !said.trim().is_empty() {
            out.push(paint(t!("分配的任务", "Its task"), bold));
            head_lines(said, TASK_LINES, &mut out);
            out.push(String::new());
        }
    }
    let tools: Vec<&crate::trace::Node> = t.nodes.iter().filter(|n| n.kind == Kind::Tool).collect();
    if tools.len() > STEPS {
        let more = tools.len() - STEPS;
        out.push(paint(&aca_core::tr!("… 前面还有 {more} 步", "… {more} earlier steps"), dim));
    }
    let name_w = tools.iter().rev().take(STEPS).map(|n| crate::trace::tool_name(&n.name).chars().count()).max().unwrap_or(0).min(10);
    for n in &tools[tools.len().saturating_sub(STEPS)..] {
        let open = n.t1 == 0 && phase == Phase::Running;
        let mark = match n.state {
            _ if open => paint("●", Style::new().fg(p.green).add_modifier(Modifier::BOLD)),
            St::Err => paint("✕", Style::new().fg(p.red)),
            St::Denied | St::Stopped => paint("■", dim),
            St::Ok => " ".to_string(),
        };
        let name = format!("{:<name_w$}", crate::trace::tool_name(&n.name));
        out.push(format!("{mark} {} {}  {}", paint(&clock(n.t0, t.tz, false), dim), paint(&name, bold), n.label));
    }
    (out, reported)
}

// ---------- 采集:哪些会话去读、读过的记着 ----------

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// Claude:记录旁边的 `subagents/` 里有 since 之后动过的记录
fn claude_stirred(record: &Path, since: u64) -> bool {
    let Ok(dir) = std::fs::read_dir(record.with_extension("").join("subagents")) else { return false };
    dir.flatten().map(|e| e.path()).any(|p| p.extension().is_some_and(|x| x == "jsonl") && mtime(&p) >= since)
}

/// Codex 的 rollout 里起没起过子线程:只找那几个词,从上次找到的地方接着找
#[derive(Default)]
struct Probe {
    path: PathBuf,
    off: u64,
    hit: bool,
}

impl Probe {
    fn spawned(&mut self, record: &Path) -> bool {
        if self.path != record {
            *self = Probe { path: record.to_path_buf(), ..Probe::default() };
        }
        if self.hit {
            return true;
        }
        let Ok(mut f) = std::fs::File::open(record) else { return false };
        let len = f.metadata().map_or(0, |m| m.len());
        // 一个词可能跨在两次读的接缝上:往回多读一点
        let from = self.off.min(len).saturating_sub(32);
        let mut buf = vec![];
        if f.seek(SeekFrom::Start(from)).is_err() || f.read_to_end(&mut buf).is_err() {
            return false;
        }
        self.off = from + buf.len() as u64;
        self.hit = ["spawn_agent", "SubAgentActivity", "sub_agent_activity"].iter().any(|w| memmem::find(&buf, w.as_bytes()).is_some());
        self.hit
    }
}

/// 一个会话读过的:它的记录读成的树,Codex 还有探到哪了;hook 报的路径用不了时找到的记录在哪(见 `record`)
#[derive(Default)]
struct Seen {
    t: Option<Trace>,
    probe: Probe,
    found: Option<(u32, Instant, Option<PathBuf>)>,
}

static SEEN: LazyLock<Mutex<HashMap<String, Seen>>> = LazyLock::new(Default::default);
const FIND_EVERY: Duration = Duration::from_secs(30);

/// 会话现在这段对话的记录:hook 报过的、还在那的用报的;不然照 `Z` 的找法(按进程认对话、到各项目目录里找)。
/// 走到第二条的不只是没带 aca 的 hook 的会话(接管来还没重启):Claude 进 / 出 worktree 时会把记录连同 `subagents/`
/// 挪到新目录对应的项目目录下,hook 报的那个路径就没了(2026-10-10 本机两个会话是这样)。
/// 找一次要查进程,找到的记 30 秒;记着的那份没了(又挪了)马上再找
fn record(s: &Session, r: &Run, seen: &mut Seen) -> Option<PathBuf> {
    let p = PathBuf::from(&r.transcript);
    if !r.transcript.is_empty() && p.is_file() {
        return Some(p);
    }
    match &seen.found {
        Some((pid, at, got)) if *pid == s.pid && at.elapsed() < FIND_EVERY && got.as_ref().is_none_or(|p| p.is_file()) => got.clone(),
        _ => {
            let got = crate::trace::find(s).ok();
            seen.found = Some((s.pid, Instant::now(), got.clone()));
            got
        }
    }
}

/// 采集完的列表上补子代理:登记的、agent 还在的 Claude / Codex 会话。`run` = 会话的运行时状态(hook 写的)
pub fn attach(sessions: &mut [Session], run: &dyn Fn(&str) -> Run) {
    let now = now_ms();
    let now_s = (now / 1000) as u64;
    let mut all = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let mut kept: HashSet<String> = HashSet::new();
    for s in sessions.iter_mut() {
        let alive = !matches!(s.state, State::Stopped | State::Crashed | State::Dead | State::Exited);
        if s.sub.is_some() || s.deck_id.is_empty() || !alive || !matches!(s.agent, Agent::Claude | Agent::Codex) {
            continue;
        }
        let seen = all.entry(s.name.clone()).or_default();
        kept.insert(s.name.clone());
        let r = run(&s.deck_id);
        let Some(path) = record(s, &r, seen) else { continue };
        let busy = matches!(s.state, State::Running | State::Ask);
        // 这一轮还在跑:从这一轮开始算;空着的只看最近动过的
        let since = now_s.saturating_sub(QUIET).min(if busy && r.turn > 0 { r.turn } else { u64::MAX });
        let stirred = match s.agent {
            Agent::Codex => seen.probe.spawned(&path) && (busy || mtime(&path) >= since),
            _ => claude_stirred(&path, since),
        };
        if !stirred {
            seen.t = None;
            continue;
        }
        if seen.t.as_ref().is_none_or(|t| t.path != path) {
            seen.t = Trace::open(path, s.agent).ok();
        } else if let Some(t) = seen.t.as_mut() {
            let _ = t.tail();
        }
        let Some(t) = &seen.t else { continue };
        s.subagents = list(t, busy, true, now);
        settle(s, now);
    }
    all.retain(|k, _| kept.contains(k));
}

/// 主对话停了、子代理还在跑:算在跑,跑了多久从最早那个还在跑的算起
pub(crate) fn settle(s: &mut Session, now: i64) {
    let first = s.subagents.iter().filter(|a| a.running()).map(|a| a.t0).min();
    if let (Some(t0), State::Idle | State::Unread) = (first, s.state) {
        s.state = State::Running;
        s.ran = ((now - t0).max(0) / 1000) as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::ts_ms;
    use crate::trace_claude::tests::with_subs;

    fn at(ms: u32) -> i64 {
        ts_ms(&crate::trace_claude::tests::at(ms))
    }

    fn names(v: &[Subagent]) -> Vec<(String, Phase)> {
        v.iter().map(|a| (a.label(), a.phase)).collect()
    }

    #[test]
    fn background_ones_run_until_their_last_word_and_the_last_turn_keeps_the_finished() {
        let (p, _, later) = with_subs("list-subs");
        // a1 还没写完(停在跑命令)
        let a1 = p.with_extension("").join("subagents/agent-a1.jsonl");
        let full = std::fs::read_to_string(&a1).unwrap();
        std::fs::write(&a1, full.lines().take(4).map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        let mut t = Trace::open_born(p.clone(), Agent::Claude, 0).unwrap();
        // 主对话这一轮早结束了(busy = false),进程还在:a1 在后台跑着
        let got = list(&t, false, true, at(6000));
        assert_eq!(got.iter().filter(|a| a.running()).map(|a| a.label()).collect::<Vec<_>>(), ["Explore · 找 width"]);
        assert!(got.iter().any(|a| a.label() == "fork · 接着想" && a.phase == Phase::Done), "{:?}", names(&got));
        // 进程没了:不会再写完,不算在跑
        assert!(list(&t, false, false, at(6000)).iter().all(|a| !a.running()));
        // 预览:派给它的话、一步一行,在跑的那步标着
        let a = got.iter().find(|a| a.running()).unwrap();
        let plain: Vec<String> = a.screen.iter().map(|l| crate::ansi::strip(l)).collect();
        assert_eq!(plain[..2], ["分配的任务", "  找出所有 width( 的调用"], "{plain:#?}");
        assert!(plain.iter().any(|l| l.contains("Bash")), "{plain:#?}");
        assert!(!a.reported && !plain.iter().any(|l| l == "返回的结果"));
        // 写完了、通知到了、开了新的一轮:那一轮里没起子代理,列表空了
        std::fs::write(&a1, &full).unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        for l in &later {
            use std::io::Write;
            writeln!(f, "{l}").unwrap();
        }
        assert!(t.tail().unwrap());
        assert_eq!(names(&list(&t, false, true, at(30_000))), []);
    }

    #[test]
    fn finished_ones_carry_their_report_and_nested_ones_follow_their_parent() {
        let (p, _, _) = with_subs("list-done");
        let t = Trace::open_born(p, Agent::Claude, 0).unwrap();
        let got = list(&t, false, true, at(6000));
        // 第一轮是最后一轮:两个都列,Explore 里再起的那个紧跟在它后面
        assert_eq!(
            names(&got),
            [
                ("Explore · 找 width".to_string(), Phase::Done),
                ("细看 ui.rs".to_string(), Phase::Done),
                ("fork · 接着想".to_string(), Phase::Done)
            ]
        );
        assert_eq!(got.iter().map(|a| a.depth).collect::<Vec<_>>(), [0, 1, 0]);
        // 跑完的:它交回来的话排最前(预览从头露),后面才是派给它的话和那几步
        let plain: Vec<String> = got[0].screen.iter().map(|l| crate::ansi::strip(l)).collect();
        assert_eq!(plain[..2], ["返回的结果", "  找到 4 处:"], "{plain:#?}");
        let k = plain.iter().position(|l| l == "分配的任务").expect("后面还有派给它的话");
        assert!(got[0].reported && k > 2 && plain[k + 1].contains("找出所有 width( 的调用"), "{plain:#?}");
        assert!(plain[k..].iter().any(|l| l.contains("Bash")), "{plain:#?}");
        assert!(got[0].tools >= 1 && got[0].dur > 0);
    }

    #[test]
    fn codex_child_threads_are_listed_the_same_way() {
        // 父线程那一轮结束了(spawn_agent 早回了),kid1 还停在跑命令:照后台的算,还在跑;kid2 那一轮结束了
        let (_h, parent) = crate::trace_codex::tests::family("list-kids", false);
        let t = Trace::open_born(parent, Agent::Codex, 0).unwrap();
        let now = ts_ms(&crate::trace_codex::tests::at(9500));
        let got = list(&t, false, true, now);
        // 行上是「名字 · 派给它的事」:老版本用 spawn_agent 的摘要(类型 · 那句话的头一行)
        assert_eq!(
            names(&got),
            [("Euclid · explorer · 看看 b.rs 里的断言".to_string(), Phase::Running), ("Noether · 查 git log".to_string(), Phase::Done)]
        );
        let plain = |a: &Subagent| a.screen.iter().map(|l| crate::ansi::strip(l)).collect::<Vec<_>>();
        let (k1, k2) = (plain(&got[0]), plain(&got[1]));
        assert_eq!(k1[..2], ["分配的任务", "  看看 b.rs 里的断言"], "{k1:#?}");
        assert!(k1.iter().any(|l| l.starts_with("● ") && l.contains("rg -n assert src/b.rs")), "{k1:#?}");
        let at = k2.iter().position(|l| l == "返回的结果").expect("跑完的写它交回来的话");
        assert_eq!(k2[at + 1], "  最近一次提交改了 b.rs", "{k2:#?}");
        // Codex 进程没了:子线程不会再写完
        assert!(list(&t, false, false, now).iter().all(|a| !a.running()));
    }

    #[test]
    fn an_idle_session_with_a_running_subagent_counts_as_running() {
        let sub = |phase, t0| Subagent {
            reported: false,
            id: "a".into(),
            kind: "Explore".into(),
            desc: "x".into(),
            phase,
            t0,
            dur: 0,
            tools: 0,
            depth: 0,
            screen: vec![],
        };
        let mut s = crate::data::tests::mk_session();
        s.state = State::Unread;
        s.subagents = vec![sub(Phase::Done, 1000), sub(Phase::Running, 5000), sub(Phase::Running, 9000)];
        settle(&mut s, 65_000);
        assert_eq!((s.state, s.ran), (State::Running, 60));
        // 在等你的不动;没有在跑的也不动
        s.state = State::Ask;
        settle(&mut s, 65_000);
        assert_eq!(s.state, State::Ask);
        s.state = State::Idle;
        s.subagents.retain(|a| !a.running());
        settle(&mut s, 65_000);
        assert_eq!(s.state, State::Idle);
    }

    /// 拿本机真的记录看(不在 CI 里跑):`ACA_SUBAGENT_PROBE=<会话的记录>[,<又一份>…] cargo test -p aca-tui probe_real_records -- --ignored --nocapture`。
    /// 两样都打出来:照它最后一刻(最后一次有动静之后一秒、进程还在)列表里会列哪些;连更早那些轮起的一共有哪些(每个的头几行预览)
    #[test]
    #[ignore]
    fn probe_real_records() {
        let Ok(paths) = std::env::var("ACA_SUBAGENT_PROBE") else { return };
        for p in paths.split(',') {
            let agent = if p.contains("rollout-") { Agent::Codex } else { Agent::Claude };
            let t0 = Instant::now();
            let t = Trace::open(p.into(), agent).unwrap();
            let took = t0.elapsed();
            let stirred = match agent {
                Agent::Codex => Probe::default().spawned(Path::new(p)),
                _ => claude_stirred(Path::new(p), 0),
            };
            let (all, now) = every(&t);
            let t1 = Instant::now();
            let listed = list(&t, false, true, now);
            println!(
                "{p}\n  读 {took:?} · 列 {:?} · 探到起过子代理 {stirred} · 最后一刻列 {} 个 · 一共 {} 个",
                t1.elapsed(),
                listed.len(),
                all.len()
            );
            for a in &all {
                let on = if listed.iter().any(|x| x.id == a.id) { "列" } else { "  " };
                println!("  {on} {:?} {}{} · {}ms · {} 步", a.phase, "↳ ".repeat(a.depth as usize), a.label(), a.dur, a.tools);
            }
            if let Some(a) = all.iter().max_by_key(|a| a.tools) {
                println!("  ---- {} 的预览({} 行)", a.label(), a.screen.len());
                for l in a.screen.iter().take(8).chain(a.screen.iter().rev().take(6).collect::<Vec<_>>().into_iter().rev()) {
                    println!("  | {}", crate::ansi::strip(l).chars().take(150).collect::<String>());
                }
            }
            assert!(all.iter().all(|a| !a.label().is_empty() && !a.id.is_empty()));
        }
    }

    /// 对着本机真的会话看(不在 CI 里跑):`ACA_SUBAGENT_LIVE=1 cargo test -p aca-tui probe_live_sessions -- --ignored --nocapture`。
    /// hook 报的路径用不了时靠 `trace::find` 找记录(见 `record`):带 hook 的会话两条路都有,这里核对它们找到的是同一份 ——
    /// hook 记的那份已经不在了(Claude 进出 worktree 挪走了)的单列,那正是要靠找的;两份都在却不一样才算错。
    /// 再假装谁都没带 hook(`Run::default()`)把子代理重新补一遍,补出来的得和带着 hook 时一样
    #[test]
    #[ignore]
    fn probe_live_sessions() {
        if std::env::var("ACA_SUBAGENT_LIVE").is_err() {
            return;
        }
        let mut v = crate::data::collect();
        let with_hook: Vec<(String, usize)> =
            v.iter().filter(|s| !s.subagents.is_empty()).map(|s| (s.name.clone(), s.subagents.len())).collect();
        let (mut same, mut moved, mut differ, mut bare) = (0, 0, vec![], 0);
        for s in
            v.iter().filter(|s| s.sub.is_none() && !s.deck_id.is_empty() && s.pid > 0 && matches!(s.agent, Agent::Claude | Agent::Codex))
        {
            let r = aca_core::status::load(&s.deck_id);
            let found = crate::trace::find(s).ok();
            match (r.transcript.is_empty(), found) {
                (true, f) => {
                    bare += 1;
                    println!("  没带 hook:{} → {}", s.title, f.map_or("找不到".into(), |p| p.display().to_string()));
                }
                (false, Some(p)) if p == Path::new(&r.transcript) => same += 1,
                (false, Some(p)) if !Path::new(&r.transcript).is_file() => {
                    moved += 1;
                    println!("  hook 记的那份不在了:{} → 找到 {}", s.title, p.display());
                }
                (false, f) => differ.push((s.title.clone(), r.transcript.clone(), f)),
            }
        }
        println!(
            "在跑的 Claude / Codex 会话:两条路找到同一份记录的 {same} 个,hook 记的不在了、靠找的 {moved} 个,没带 hook 的 {bare} 个,不一样的 {} 个",
            differ.len()
        );
        for d in &differ {
            println!("  不一样:{d:?}");
        }
        for s in &mut v {
            s.subagents.clear();
        }
        attach(&mut v, &|_| Run::default());
        let bare_way: Vec<(String, usize)> =
            v.iter().filter(|s| !s.subagents.is_empty()).map(|s| (s.name.clone(), s.subagents.len())).collect();
        println!("带 hook 时补上子代理的会话 {with_hook:?}\n当成没带 hook 再补一遍 {bare_way:?}");
        assert!(differ.is_empty());
        assert_eq!(with_hook, bare_way);
    }

    #[test]
    fn codex_probe_finds_a_spawn_word_across_two_reads() {
        let dir = std::env::temp_dir().join(format!("aca-subagent-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("rollout.jsonl");
        std::fs::write(&p, "{\"type\":\"x\"}\n{\"name\":\"spawn_").unwrap();
        let mut probe = Probe::default();
        assert!(!probe.spawned(&p));
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        use std::io::Write;
        writeln!(f, "agent\"}}").unwrap();
        assert!(probe.spawned(&p));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
