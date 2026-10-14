//! 会话记录(`Z`):把一段对话的记录文件拼成一棵树 —— 一轮 → 这一轮的每次模型调用 → 调用发起的工具,
//! 像 Langfuse / Phoenix 的 trace。只读本地的记录文件,不联网、不改文件。
//! 界面(ui.rs):桌面左边是对话(`chat`:一轮一块,你说的、Claude 最后说的、一行数字),
//! 右边是选中那一轮的中间过程(`steps`:每次调用的数字、想的、中间说的、工具带改动和输出);手机上两栏各占整屏。
//!
//! 认 Claude(`projects/<目录>/<会话 id>.jsonl`,解析在 `trace_claude`)和 Codex(`sessions/…/rollout-*.jsonl`,`trace_codex`)。
//! 节点里只放画一行要的东西;正文(提问全文、回复、工具的输入输出)不常驻,要看时按记下的偏移从文件里读回来,
//! 两家各自读回来整理成一样的(提问、想的和说的、工具的 `Io`),再由 `trace_page` 排成字。
//! 在跑的会话:文件变长了只读新增的那截(写到一半的最后一行下次再读)。
//!
//! 子代理(Claude 的 `subagents/agent-*.jsonl`、Codex 的子线程)是另一份记录,各自读成一棵树(`Sub`),
//! 挂在发起它的那个工具(Agent / spawn_agent)下;它的钱算进那一轮。
//!
//! 钱的口径同 `$`:按官方 API 价折美元(`usage::call_cost`);接续 / 分叉 / 子线程开头抄过来的历史不计,
//! 被回退的轮照算(钱花出去了),只是不编号。
//! 耗时:一轮的是记录里写明的(Claude 的 turn_duration、Codex 的 duration_ms),工具的是结果那行减发起那行
//! (Codex 的命令有写明的用写明的),
//! 一次调用的是拿时间戳估的(记录里没有首字、重试)。

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use aca_core::i18n::Tr;
use aca_core::{t, tr};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ansi::{Seg, Styled};
use crate::data::{Agent, Session};
use crate::theme;
use crate::ui::{clip, dim};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 一轮:一次提问(或斜杠命令、后台任务的完成通知)到这一轮结束
    Turn,
    /// 一次模型调用
    Call,
    /// 调用发起的工具
    Tool,
    /// API 出错(Claude 记成一条假回复):不算调用
    Fail,
    /// 只占一行的标记:压缩
    Mark,
    /// 接续 / 分叉时从上一段对话抄过来的那几轮,折在这一行下面,不计
    Copied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum St {
    #[default]
    Ok,
    Err,
    /// 权限被拒
    Denied,
    /// 被打断
    Stopped,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: Kind,
    pub parent: Option<usize>,
    pub kids: Vec<usize>,
    /// 毫秒;t1 = 0:还没结束(或者记录里没写结束)
    pub t0: i64,
    pub t1: i64,
    /// 耗时是记录里写明的,不是时间戳相减估的
    pub exact: bool,
    /// 工具的 tool_use id / 调用的 message.id(详情里找回原文用)
    pub id: String,
    /// 工具名 / 斜杠命令名
    pub name: String,
    /// 一行摘要(调用:说的第一句)
    pub label: String,
    /// 调用:想的第一句(记录里存了思考才有)
    pub thought: String,
    /// 输入(不含缓存)、5 分钟缓存写、1 小时缓存写、缓存读、输出(同 usage.rs)
    pub tok: [u64; 5],
    /// 输出里的思考
    pub think: u64,
    pub searches: u64,
    /// 快速模式、只在美国推理(加价)
    pub fast: bool,
    pub us: bool,
    /// Trace.models 的下标
    pub model: usize,
    pub effort: String,
    pub stop: String,
    pub state: St,
    pub copied: bool,
    /// 一轮:后来被回退了(Codex 的 thread_rolled_back):画灰、不编号,钱照算
    pub rolled: bool,
    /// 工具:它起的子代理(Trace.subs 的下标)
    pub sub: Option<usize>,
    /// 钩子花的时间(毫秒)
    pub hook: u64,
    /// 从哪几行来的:(偏移, 长度)。工具:第一行是发起它的回复,第二行是结果
    pub src: Vec<(u64, u32)>,
}

impl Node {
    fn new(kind: Kind, parent: Option<usize>, t0: i64) -> Node {
        Node {
            kind,
            parent,
            kids: vec![],
            t0,
            t1: 0,
            exact: false,
            id: String::new(),
            name: String::new(),
            label: String::new(),
            thought: String::new(),
            tok: [0; 5],
            think: 0,
            searches: 0,
            fast: false,
            us: false,
            model: 0,
            effort: String::new(),
            stop: String::new(),
            state: St::Ok,
            copied: false,
            rolled: false,
            sub: None,
            hook: 0,
            src: vec![],
        }
    }

    /// 上下文:这次请求送进去多少(输入 + 缓存写 + 缓存读)
    pub fn context(&self) -> u64 {
        self.tok[0] + self.tok[1] + self.tok[2] + self.tok[3]
    }
}

/// 一段对话的树
pub struct Trace {
    pub path: PathBuf,
    pub agent: Agent,
    pub nodes: Vec<Node>,
    /// 顶层:一轮、标记、抄过来的那一块,按先后
    pub roots: Vec<usize>,
    pub models: Vec<String>,
    /// 它最近一次记的目录(工具摘要里的路径按它写成相对的)
    pub cwd: String,
    /// 本地时区(秒),写时刻用
    pub tz: i64,
    /// 读到哪(整行为止)
    pub(crate) off: u64,
    /// 文件创建的时刻(毫秒,0 = 不知道):开头比它早的对话行是接续时抄过来的
    pub(crate) born: i64,
    /// 还在文件开头抄过来的那一段里
    pub(crate) lead: bool,
    // ---- 解析到一半的状态(跨着读新增的那截) ----
    /// 现在这一轮
    pub(crate) turn: Option<usize>,
    /// message.id → 调用
    pub(crate) calls: HashMap<String, usize>,
    /// tool_use id → 工具
    pub(crate) tools: HashMap<String, usize>,
    /// 每行 uuid → 时刻:一次调用从它的第一块的上一行算起
    pub(crate) at: HashMap<u64, i64>,
    /// Codex 解析到一半的状态
    pub(crate) cx: crate::trace_codex::State,
    /// 这份记录里起的子代理
    pub subs: Vec<Sub>,
    /// 是子代理的记录(Claude 的子代理不再往下找 subagents 目录:嵌套的由最上面那份按 parentAgentId 分下来)
    pub(crate) is_sub: bool,
    /// 找过的子代理 id(Claude:最上面那份记着所有层的;Codex:各记各的)
    pub(crate) sub_ids: std::collections::HashSet<String>,
    /// 还没找到记录的子代理:(工具, 子代理 id, 找过几回)。起的时候文件可能还没写出来,下次读新增的时候再找
    pub(crate) want: Vec<(usize, String, u8)>,
    /// 后台任务的完成通知:任务 id(= 子代理 id)→ 结果(完成 / 失败 / 被停)
    pub(crate) done: HashMap<String, St>,
}

/// 一个子代理:它自己的记录读成的树
pub struct Sub {
    /// Claude 的 agentId / Codex 的线程 id
    pub id: String,
    /// 类型(Explore、general-purpose / Codex 的 explorer、worker)
    pub kind: String,
    /// 说明(Claude 的 description / Codex 的名字)
    pub desc: String,
    pub t: Trace,
}

impl Trace {
    pub fn new(path: PathBuf, agent: Agent) -> Trace {
        Trace {
            path,
            agent,
            nodes: vec![],
            roots: vec![],
            models: vec![String::new()],
            cwd: String::new(),
            tz: 0,
            off: 0,
            born: 0,
            lead: true,
            turn: None,
            calls: HashMap::new(),
            tools: HashMap::new(),
            at: HashMap::new(),
            cx: crate::trace_codex::State::default(),
            subs: vec![],
            is_sub: false,
            sub_ids: std::collections::HashSet::new(),
            want: vec![],
            done: HashMap::new(),
        }
    }

    /// 读一份子代理的记录。Codex 的给上父线程的轮(开头抄过来的父线程历史按它认)
    pub(crate) fn open_sub(path: PathBuf, agent: Agent, parent_turns: Option<std::collections::HashSet<String>>) -> std::io::Result<Trace> {
        let mut t = Trace::new(path, agent);
        t.tz = crate::usage::local_offset();
        // 子代理的记录开头不会抄上一段(本机 175 份都没有):不按文件创建的时刻找抄的
        t.is_sub = true;
        t.cx.parent_turns = parent_turns;
        t.tail()?;
        Ok(t)
    }

    /// 读整份记录
    pub fn open(path: PathBuf, agent: Agent) -> std::io::Result<Trace> {
        let born = born(&path);
        Trace::open_born(path, agent, born)
    }

    pub(crate) fn open_born(path: PathBuf, agent: Agent, born: i64) -> std::io::Result<Trace> {
        let mut t = Trace::new(path, agent);
        t.tz = crate::usage::local_offset();
        t.born = born;
        t.tail()?;
        Ok(t)
    }

    /// 读新增的那截(连同子代理的);有新东西返回 true。写到一半的最后一行留到下次
    pub fn tail(&mut self) -> std::io::Result<bool> {
        let mut grew = self.tail_file()?;
        for s in &mut self.subs {
            grew |= s.t.tail().unwrap_or(false);
        }
        grew |= match self.agent {
            Agent::Claude => crate::trace_claude::find_subs(self),
            Agent::Codex => crate::trace_codex::find_subs(self),
            _ => false,
        };
        Ok(grew)
    }

    fn tail_file(&mut self) -> std::io::Result<bool> {
        let mut f = File::open(&self.path)?;
        let len = f.metadata()?.len();
        if len < self.off {
            // 文件变短了(被换掉了):从头读
            let (path, agent, tz, is_sub) = (self.path.clone(), self.agent, self.tz, self.is_sub);
            let parent_turns = self.cx.parent_turns.take();
            *self = Trace::new(path, agent);
            self.tz = tz;
            self.born = if is_sub { 0 } else { born(&self.path) };
            self.is_sub = is_sub;
            self.cx.parent_turns = parent_turns;
        }
        if len == self.off {
            return Ok(false);
        }
        f.seek(SeekFrom::Start(self.off))?;
        let mut r = BufReader::with_capacity(1 << 16, f);
        let mut buf = Vec::with_capacity(1 << 16);
        let start = self.off;
        loop {
            buf.clear();
            let n = r.read_until(b'\n', &mut buf)?;
            if n == 0 || buf.last() != Some(&b'\n') {
                break;
            }
            let at = self.off;
            self.off += n as u64;
            let line = &buf[..n - 1];
            match self.agent {
                _ if line.is_empty() => {}
                Agent::Claude => crate::trace_claude::line(self, line, at),
                Agent::Codex => crate::trace_codex::line(self, line, at),
                _ => {}
            }
        }
        Ok(self.off != start)
    }

    pub(crate) fn add(&mut self, kind: Kind, parent: Option<usize>, t0: i64, copied: bool) -> usize {
        let i = self.nodes.len();
        let mut n = Node::new(kind, parent, t0);
        n.copied = copied;
        self.nodes.push(n);
        match parent {
            Some(p) => self.nodes[p].kids.push(i),
            None => self.roots.push(i),
        }
        i
    }

    /// 顶层加一个(一轮 / 标记):抄过来的放进最后那一块「接着上一段对话」里
    pub(crate) fn add_top(&mut self, kind: Kind, t0: i64, copied: bool) -> usize {
        if !copied {
            return self.add(kind, None, t0, false);
        }
        let group = match self.roots.last() {
            Some(&g) if self.nodes[g].kind == Kind::Copied => g,
            _ => self.add(Kind::Copied, None, t0, true),
        };
        self.add(kind, Some(group), t0, true)
    }

    /// 被打断时还没回结果的工具:算停在打断那一刻
    pub(crate) fn stop_open_tools(&mut self, turn: usize, ts: i64) {
        for c in self.nodes[turn].kids.clone() {
            for k in self.nodes[c].kids.clone() {
                let n = &mut self.nodes[k];
                if n.kind == Kind::Tool && n.t1 == 0 {
                    n.t1 = ts.max(n.t0);
                    n.state = St::Stopped;
                }
            }
        }
    }

    pub(crate) fn model_idx(&mut self, name: &str) -> usize {
        match self.models.iter().position(|m| m == name) {
            Some(i) => i,
            None => {
                self.models.push(name.to_string());
                self.models.len() - 1
            }
        }
    }

    /// 出现最多的模型(调用行上只在不是它时写模型名)
    pub fn main_model(&self) -> usize {
        let mut n = vec![0usize; self.models.len()];
        for x in self.nodes.iter().filter(|x| x.kind == Kind::Call) {
            n[x.model] += 1;
        }
        n.iter().enumerate().max_by_key(|(_, c)| **c).map_or(0, |(i, _)| i)
    }

    /// 这次调用自己的钱;价目表里没有这个模型 = None
    pub fn cost(&self, i: usize) -> Option<f64> {
        let n = &self.nodes[i];
        crate::usage::call_cost(&self.models[n.model], n.tok, n.fast, n.us, n.searches)
    }

    /// 一个节点连同底下的合计(抄过来的不计)
    pub fn sum(&self, i: usize) -> Sum {
        let mut s = Sum::default();
        self.add_up(i, &mut s);
        s
    }

    fn add_up(&self, i: usize, s: &mut Sum) {
        let n = &self.nodes[i];
        if n.copied {
            return;
        }
        match n.kind {
            Kind::Call => {
                s.calls += 1;
                for k in 0..5 {
                    s.tok[k] += n.tok[k];
                }
                s.out += n.tok[4];
                s.ctx = s.ctx.max(n.context());
                match self.cost(i) {
                    Some(c) => s.cost += c,
                    None => s.unpriced = true,
                }
            }
            Kind::Tool => {
                s.tools += 1;
                // 子代理:钱、token 算进来,调用、工具、上下文是它自己的事(右边那一步上写)
                if let Some(sub) = n.sub.and_then(|k| self.subs.get(k)) {
                    let x = sub.t.total(false, 0).2;
                    s.subs += 1 + x.subs;
                    s.cost += x.cost;
                    s.sub_cost += x.cost;
                    s.unpriced |= x.unpriced;
                    for k in 0..5 {
                        s.tok[k] += x.tok[k];
                    }
                }
            }
            _ => {}
        }
        for &k in &n.kids {
            self.add_up(k, s);
        }
    }

    /// 一个工具起的子代理
    pub fn sub_of(&self, i: usize) -> Option<&Sub> {
        self.nodes.get(i)?.sub.and_then(|k| self.subs.get(k))
    }

    /// 顺着起子代理的工具一层层进去
    pub fn at(&self, path: &[usize]) -> &Trace {
        match path.split_first() {
            Some((&i, rest)) => self.sub_of(i).map_or(self, |s| s.t.at(rest)),
            None => self,
        }
    }

    /// 进到那一层还在不在跑(每一层的子代理都还在干活)
    pub fn live_at(&self, path: &[usize], live: bool, now: i64) -> bool {
        match path.split_first() {
            Some((&i, rest)) => self.sub_of(i).is_some_and(|s| s.t.live_at(rest, self.sub_live(i, live, now), now)),
            None => live,
        }
    }

    /// 连同子代理的记录读到哪了(子代理长了,挂着它的那一块要重排)
    pub fn grown(&self) -> u64 {
        self.off + self.subs.iter().map(|s| s.t.grown()).sum::<u64>()
    }

    /// 一轮里起的子代理:(工具, 子代理)
    pub fn subs_in(&self, turn: usize) -> Vec<usize> {
        let mut out = vec![];
        for &c in &self.nodes[turn].kids {
            out.extend(self.nodes[c].kids.iter().copied().filter(|&k| self.nodes[k].sub.is_some()));
        }
        out
    }

    /// 子代理还在不在干活:会话在跑、没收到完成通知、它自己也还没写完(Claude 最后一次调用停在 end_turn,
    /// Codex 的每一轮都结束了),而且半小时内还有动静 —— 会话被关掉时在跑的子代理不会再写完,也没有通知
    /// (本机 4 个停在工具调用之后),会话再开起来时不能一直算在跑
    pub fn sub_live(&self, i: usize, live: bool, now: i64) -> bool {
        let Some(s) = self.sub_of(i) else { return false };
        live && !self.done.contains_key(&s.id) && !s.t.finished() && now - s.t.last_active() < SUB_QUIET
    }

    /// 最后一次有动静的时刻
    pub(crate) fn last_active(&self) -> i64 {
        self.nodes.iter().map(|n| n.t0.max(n.t1)).max().unwrap_or(0)
    }

    /// 这份记录写完了(子代理用):最后一次调用说完了话、没有还没回结果的工具;Codex 的每一轮都结束了
    pub(crate) fn finished(&self) -> bool {
        let Some(turn) = self.last_turn() else { return false };
        let n = &self.nodes[turn];
        if self.agent == Agent::Codex {
            return n.t1 > 0;
        }
        n.t1 > 0
            || n.kids
                .iter()
                .rev()
                .find(|&&k| self.nodes[k].kind == Kind::Call)
                .is_some_and(|&c| self.nodes[c].stop == "end_turn" && self.nodes[c].kids.is_empty())
    }

    /// 子代理的结果:通知里写的(失败 / 被停),没有就是它自己的最后一轮的
    pub fn sub_state(&self, i: usize) -> St {
        let Some(s) = self.sub_of(i) else { return St::Ok };
        match self.done.get(&s.id) {
            Some(&st) => st,
            None => s.t.last_turn().map_or(St::Ok, |r| s.t.nodes[r].state),
        }
    }

    /// 子代理从头到尾多久(后台跑的:Agent 工具早就返回了,真正的耗时在这里)
    pub fn sub_dur(&self, i: usize, live: bool, now: i64) -> i64 {
        let Some(s) = self.sub_of(i) else { return 0 };
        let live = self.sub_live(i, live, now);
        let first = s.t.roots.iter().map(|&r| s.t.nodes[r].t0).find(|&x| x > 0).unwrap_or(0);
        let last = if live { now } else { s.t.roots.iter().map(|&r| s.t.last_at(r)).max().unwrap_or(first) };
        (last - first).max(0)
    }

    /// 最后一次有动静的时刻(自己和底下的)
    pub fn last_at(&self, i: usize) -> i64 {
        let n = &self.nodes[i];
        n.kids.iter().map(|&k| self.last_at(k)).fold(n.t0.max(n.t1), i64::max)
    }

    /// 耗时(毫秒)。没结束的:在跑就算到现在,不然算到最后一次有动静
    pub fn dur(&self, i: usize, live: bool, now: i64) -> i64 {
        let n = &self.nodes[i];
        let end = match n.t1 {
            0 if live => now,
            0 => self.last_at(i),
            t => t,
        };
        (end - n.t0).max(0)
    }

    /// 顶层的第几轮(抄过来的不编号)
    pub fn turn_no(&self, i: usize) -> Option<usize> {
        let n = &self.nodes[i];
        if n.kind != Kind::Turn || n.copied || n.rolled {
            return None;
        }
        self.roots.iter().filter(|&&r| self.nodes[r].kind == Kind::Turn && !self.nodes[r].rolled).position(|&r| r == i).map(|p| p + 1)
    }

    /// 最后一轮(自己编过号的)
    pub fn last_turn(&self) -> Option<usize> {
        self.roots.iter().rev().copied().find(|&r| self.nodes[r].kind == Kind::Turn)
    }

    /// 这个节点在不在跑:会话在跑、它是最后一轮(或在最后一轮里),而且还没结束
    pub fn running(&self, i: usize, live: bool) -> bool {
        if !live || self.nodes[i].t1 != 0 || self.nodes[i].copied {
            return false;
        }
        let mut top = i;
        while let Some(p) = self.nodes[top].parent {
            top = p;
        }
        Some(top) == self.last_turn()
    }

    /// 全部:几轮、干活的时间(各轮耗时加起来)、合计
    pub fn total(&self, live: bool, now: i64) -> (usize, i64, Sum) {
        let mut s = Sum::default();
        let mut turns = 0;
        let mut dur = 0;
        for &r in &self.roots {
            if self.nodes[r].kind == Kind::Turn {
                turns += usize::from(!self.nodes[r].rolled);
                dur += self.dur(r, self.running(r, live), now);
            }
            self.add_up(r, &mut s);
        }
        (turns, dur, s)
    }

    /// 读回一行原文
    pub fn read_at(&self, (off, len): (u64, u32)) -> Option<serde_json::Value> {
        let mut f = File::open(&self.path).ok()?;
        f.seek(SeekFrom::Start(off)).ok()?;
        let mut buf = vec![0; len as usize];
        f.read_exact(&mut buf).ok()?;
        serde_json::from_slice(&buf).ok()
    }
}

/// 文件创建的时刻(毫秒)。修改时间比创建还早(`cp -p` 拷过来的)时不信它:返回 0,不找抄过来的
fn born(path: &Path) -> i64 {
    let ms = |t: std::time::SystemTime| t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    let Ok(m) = std::fs::metadata(path) else { return 0 };
    match (m.created(), m.modified()) {
        (Ok(c), Ok(w)) if ms(w) >= ms(c) => ms(c),
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sum {
    pub calls: usize,
    pub tools: usize,
    pub tok: [u64; 5],
    pub out: u64,
    /// 最大的一次上下文
    pub ctx: u64,
    pub cost: f64,
    /// 有价目表里没有的模型:钱只是能算的那部分
    pub unpriced: bool,
    /// 底下起了几个子代理(连它们再起的),它们花的钱(已经算在 cost 里)
    pub subs: usize,
    pub sub_cost: f64,
}

/// 会话 → 它现在这段对话的记录文件
pub fn find(s: &Session) -> Result<PathBuf, String> {
    if !matches!(s.agent, Agent::Claude | Agent::Codex) {
        return Err(t!("不是 Claude / Codex 会话", "not a Claude / Codex session").into());
    }
    let conf = crate::mcp::home_of(s);
    let w = crate::workdir::Where::of(s);
    crate::workdir::log_of(&w, &conf).ok_or_else(|| t!("找不到记录", "no record found").into())
}

/// 找到并读完这个会话的记录
pub fn load(s: &Session) -> Result<Trace, String> {
    let t = Trace::open(find(s)?, s.agent).map_err(|_| t!("读不了记录", "can't read the record").to_string())?;
    if t.cx.old {
        return Err(t!("这份记录太老(Codex 0.107 以前的格式)", "this record is too old (the format before Codex 0.107)").into());
    }
    Ok(t)
}

/// 一行摘要最多这么多字
pub(crate) const LABEL: usize = 200;

/// 子代理这么久(毫秒)没动静就不算在跑了(见 Trace::sub_live)
const SUB_QUIET: i64 = 30 * 60_000;

/// 2026-09-30T01:02:03.456Z → 毫秒
pub(crate) fn ts_ms(s: &str) -> i64 {
    let ms = s.get(19..).and_then(|r| r.strip_prefix('.')).map_or(0, |r| {
        let d: String = r.chars().take_while(char::is_ascii_digit).take(3).collect();
        format!("{d:0<3}").parse::<i64>().unwrap_or(0)
    });
    crate::usage::parse_ts(s) * 1000 + ms
}

/// 列表里的一句不带 Markdown 记号:**、`、行首的 # 和 -
pub(crate) fn unmark(s: &str) -> String {
    let l = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let l = l.trim_start_matches('#').trim_start();
    let l = l.strip_prefix("- ").unwrap_or(l);
    l.replace("**", "").replace('`', "")
}

/// 一段 Markdown 摊成一行:每行去掉记号接起来。行尾没有句末标点的(标题)后面用 · 隔开
pub(crate) fn flat(s: &str) -> String {
    let mut out = String::new();
    for l in s.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match out.chars().last() {
            None => {}
            Some('。' | '！' | '？' | '：' | '；' | '，' | '、' | '…' | '）' | '」' | '”') => {}
            Some('.' | '!' | '?' | ':' | ';' | ',' | ')' | '"') => out.push(' '),
            Some(_) => out.push_str(" · "),
        }
        out.push_str(&unmark(l));
    }
    out
}

// ---------- 数字 ----------

pub fn fmt_dur(ms: i64) -> String {
    if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 3_600_000 {
        format!("{}m{:02}s", ms / 60_000, ms / 1000 % 60)
    } else {
        format!("{}h{:02}m", ms / 3_600_000, ms / 60_000 % 60)
    }
}

pub fn fmt_tok(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else if n < 1_000_000 {
        format!("{}k", n / 1000)
    } else {
        format!("{:.1}M", n as f64 / 1e6)
    }
}

pub fn fmt_cost(c: f64) -> String {
    if c >= 100.0 { format!("${c:.0}") } else { format!("${c:.2}") }
}

/// 千分位:1,204
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// 本地时刻 HH:MM(secs = true 带秒)
pub fn clock(ms: i64, tz: i64, secs: bool) -> String {
    let t = (ms / 1000 + tz).rem_euclid(86400);
    if secs { format!("{:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60) } else { format!("{:02}:{:02}", t / 3600, t / 60 % 60) }
}

// ---------- 一行的 ----------

/// 节点所在的那一轮(抄过来的那一块里的轮也算一轮)
pub fn turn_of(t: &Trace, mut i: usize) -> usize {
    while let Some(p) = t.nodes[i].parent.filter(|&p| t.nodes[p].kind != Kind::Copied) {
        i = p;
    }
    i
}

/// 一行拼好:左边几段 + 中间摘要(截到放得下)
fn fit(left: Vec<Span<'static>>, label: &str, label_st: Style, w: usize) -> Line<'static> {
    let lw: usize = left.iter().map(|s| s.content.width()).sum();
    let mut v = left;
    v.push(Span::styled(clip(label, w.saturating_sub(lw)).trim_end().to_string(), label_st));
    Line::from(v)
}

/// 工具名写短:MCP 的写 MCP(服务 · 工具在摘要里)
pub(crate) fn tool_name(name: &str) -> &str {
    if name.starts_with("mcp__") { "MCP" } else { name }
}

/// 对话里只占一行的:压缩、抄过来的那一块(能展开)
pub fn row(t: &Trace, i: usize, opened: bool, w: usize) -> Line<'static> {
    let n = &t.nodes[i];
    match n.kind {
        Kind::Copied => {
            let turns = n.kids.iter().filter(|&&k| t.nodes[k].kind == Kind::Turn).count();
            let fold = Span::styled(if opened { "▾ " } else { "▸ " }, theme::faint());
            fit(
                vec![Span::raw(" "), fold],
                &tr!("接着上一段对话 · {turns} 轮 · 不计", "continued from the previous conversation · {turns} turns · not counted"),
                dim(),
                w,
            )
        }
        _ => fit(vec![Span::raw("   "), Span::styled("── ", theme::faint())], &n.label, dim(), w),
    }
}

/// 模型名写短:claude-haiku-4-5-20251001 → haiku-4-5
pub(crate) fn short_model(m: &str) -> String {
    let k = crate::usage::model_key(m);
    k.strip_prefix("claude-").unwrap_or(&k).to_string()
}

// ---------- 右边 / 详情:一段段的字 ----------

/// 一段字:body 按宽度折行,第一行前面接 first,折下去的行前面接 rest(缩进、代码的竖线)。
/// 有 right(靠右的一截)或 fill(中间填满的字,分隔线的 ─)的只占一行,放不下的截掉
#[derive(Clone, Debug, Default)]
pub struct Para {
    pub first: Styled,
    pub rest: Styled,
    pub body: Styled,
    pub right: Styled,
    pub fill: Option<Seg>,
    /// 折好的每行补空格到行尾,铺这个底色(改动里加的 / 删的那行)
    pub pad_bg: Option<ratatui::style::Color>,
    /// 折好最多几行(0 = 不限),多的不要,最后一行末尾写 …:按行数截,窄屏和宽屏占的高度一样
    pub max: usize,
}

impl Para {
    pub fn new(body: Styled) -> Para {
        Para { body, ..Para::default() }
    }

    /// 每行前面都接同一段(缩进 / 竖线)
    pub fn under(pre: Styled, body: Styled) -> Para {
        Para { first: pre.clone(), rest: pre, body, ..Para::default() }
    }

    /// 第一行接 first,折下去的行接 rest
    pub fn hung(first: Styled, rest: Styled, body: Styled) -> Para {
        Para { first, rest, body, ..Para::default() }
    }
}

/// 一串段落前面再接一层:第一段的第一行接 first,别的行接 rest
pub(crate) fn hang(ps: Vec<Para>, first: &Styled, rest: &Styled) -> Vec<Para> {
    ps.into_iter()
        .enumerate()
        .map(|(k, p)| Para {
            first: [if k == 0 { first } else { rest }.clone(), p.first].concat(),
            rest: [rest.clone(), p.rest].concat(),
            ..p
        })
        .collect()
}

fn width_of(s: &Styled) -> usize {
    s.iter().map(|x| x.text.width()).sum()
}

/// 截到 w 格,截掉了的末尾写 …
fn cut(s: &Styled, w: usize) -> Styled {
    if width_of(s) <= w {
        return s.clone();
    }
    let (mut out, mut n) = (vec![], 0);
    for x in s {
        let mut text = String::new();
        for ch in x.text.chars() {
            let cw = ch.width().unwrap_or(0);
            if n + cw + 1 > w {
                break;
            }
            text.push(ch);
            n += cw;
        }
        let full = text.len() == x.text.len();
        out.push(Seg { text, ..x.clone() });
        if !full {
            break;
        }
    }
    if w > 0 {
        out.push(seg("…", theme::faint()));
    }
    out
}

/// 按宽度折好
pub fn wrap(ps: &[Para], w: usize) -> Vec<Styled> {
    wrap_clipped(ps, w).0
}

/// 同 `wrap`,另外说有没有哪段因为 max 截掉了行(这个宽度下截了,那一步才有东西可展开)
pub fn wrap_clipped(ps: &[Para], w: usize) -> (Vec<Styled>, bool) {
    let (mut out, mut clipped) = (vec![], false);
    for p in ps {
        if !p.right.is_empty() || p.fill.is_some() {
            // 一行:左边截到放得下,靠右的放不下就不要了
            let room = w.saturating_sub(width_of(&p.first));
            let rw = width_of(&p.right);
            let right = rw > 0 && room >= rw + 8;
            let left = if right { room - rw - 1 } else { room };
            let body = cut(&p.body, left);
            let gap = left - width_of(&body);
            let mut line = [p.first.clone(), body].concat();
            match &p.fill {
                Some(f) => line.push(Seg { text: f.text.repeat(gap), ..f.clone() }),
                None if right => line.push(seg(" ".repeat(gap), Style::new())),
                None => {}
            }
            if right {
                line.push(seg(" ", Style::new()));
                line.extend(p.right.iter().cloned());
            }
            out.push(line);
            continue;
        }
        let room = w.saturating_sub(width_of(&p.first).max(width_of(&p.rest))).max(1);
        let mut rows = crate::ansi::wrap(std::slice::from_ref(&p.body), room);
        if p.max > 0 && rows.len() > p.max {
            rows.truncate(p.max);
            let last = rows.pop().unwrap_or_default();
            rows.push(cut(&[last, vec![seg("…", theme::faint())]].concat(), room));
            clipped = true;
        }
        for (k, l) in rows.into_iter().enumerate() {
            let mut line = if k == 0 { p.first.clone() } else { p.rest.clone() };
            line.extend(l);
            if let Some(bg) = p.pad_bg {
                line.push(seg(" ".repeat(w.saturating_sub(width_of(&line))), Style::new().bg(bg)));
            }
            out.push(line);
        }
    }
    (out, clipped)
}

pub(crate) fn seg(text: impl Into<String>, style: Style) -> Seg {
    Seg { text: text.into(), style, link: None }
}

pub(crate) fn minor(text: impl Into<String>) -> Para {
    Para::new(vec![seg(text, dim())])
}

pub(crate) fn blank() -> Para {
    Para::default()
}

/// 分节的标题:── 回复 ──
pub(crate) fn heading(text: &str) -> Para {
    Para::new(vec![seg("── ", theme::faint()), seg(text, Style::new().add_modifier(Modifier::BOLD)), seg(" ──", theme::faint())])
}

/// 工具输出最多放这么多行
pub const OUT_LINES: usize = 200;

/// 一行里的控制字符(\r、ESC 的颜色码)去掉,Tab 换成空格
pub(crate) fn clean(l: &str) -> String {
    crate::ansi::strip(l).replace('\t', "    ").chars().filter(|c| !c.is_control()).collect()
}

/// 整段文字按行放进去;limit = Some(n) 时只放前 n 行,标题里写共几行
pub(crate) fn body(out: &mut Vec<Para>, title: &str, text: &str, limit: Option<usize>, st: Style) {
    body_hl(out, title, text, limit, st, None);
}

/// 同 body,代码按 hl 上色(命令、JSON)
pub(crate) fn body_hl(out: &mut Vec<Para>, title: &str, text: &str, limit: Option<usize>, st: Style, mut hl: Option<crate::hl::Hl>) {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    match limit {
        Some(n) if total > n => {
            out.push(heading(&tr!("{title} · 前 {n} 行 / 共 {} 行", "{title} · first {n} of {} lines", thousands(total as u64))))
        }
        _ => out.push(heading(title)),
    }
    for l in lines.into_iter().take(limit.unwrap_or(usize::MAX)) {
        let l = clean(l);
        out.push(Para::new(match hl.as_mut() {
            Some(h) => h.line(&l, st),
            None => vec![seg(l, st)],
        }));
    }
}

/// Markdown 排成段落:标题、**粗体**、`代码`、列表、引用、代码块(竖线)、表格的分隔行
/// 代码块按写的语言上色(淡的段落里的不上色,整段都淡)
pub(crate) fn md(text: &str, st: Style) -> Vec<Para> {
    let mut out = vec![];
    let mut code = false;
    let mut hl: Option<crate::hl::Hl> = None;
    for raw in text.lines() {
        let line = clean(raw);
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            code = !code;
            hl = if code && st.fg.is_none() { crate::hl::Hl::for_lang(t.trim_start_matches(['`', '~'])) } else { None };
            continue;
        }
        if code {
            let body = match hl.as_mut() {
                Some(h) => h.line(&line, st),
                None => vec![seg(line.clone(), st)],
            };
            out.push(Para::under(vec![seg("│ ", theme::faint())], body));
            continue;
        }
        let pad = " ".repeat(line.len() - t.len());
        if let Some(h) = heading_text(t) {
            out.push(Para::new(inline(h, st.add_modifier(Modifier::BOLD))));
        } else if t.starts_with('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
            out.push(Para::new(vec![seg(line.clone(), theme::faint())]));
        } else if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            out.push(Para::hung(vec![seg(format!("{pad}• "), theme::faint())], vec![seg(format!("{pad}  "), st)], inline(rest, st)));
        } else if let Some((num, rest)) = ordered(t) {
            let rest_pad = " ".repeat(pad.len() + num.len() + 1);
            out.push(Para::hung(vec![seg(format!("{pad}{num} "), dim())], vec![seg(rest_pad, st)], inline(rest, st)));
        } else if let Some(q) = t.strip_prefix('>') {
            out.push(Para::under(vec![seg(format!("{pad}│ "), theme::faint())], inline(q.trim_start(), dim())));
        } else {
            out.push(Para::new(inline(&line, st)));
        }
    }
    out
}

/// "## 标题" → "标题"
fn heading_text(t: &str) -> Option<&str> {
    let h = t.trim_start_matches('#');
    let n = t.len() - h.len();
    ((1..=6).contains(&n) && h.starts_with(' ')).then(|| h.trim())
}

/// "12. 下一步" → ("12.", "下一步")
fn ordered(t: &str) -> Option<(&str, &str)> {
    let d = t.find(|c: char| !c.is_ascii_digit())?;
    if d == 0 || d > 3 {
        return None;
    }
    let rest = t[d..].strip_prefix(". ").or_else(|| t[d..].strip_prefix(") "))?;
    Some((&t[..=d], rest))
}

/// 行内的 **粗体**、`代码`(去掉记号)
fn inline(s: &str, st: Style) -> Styled {
    let mut out: Styled = vec![];
    let (mut bold, mut code) = (false, false);
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Styled, bold: bool, code: bool| {
        if !cur.is_empty() {
            let style = if code {
                theme::accent()
            } else if bold {
                st.add_modifier(Modifier::BOLD)
            } else {
                st
            };
            out.push(seg(std::mem::take(cur), style));
        }
    };
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            flush(&mut cur, &mut out, bold, code);
            code = !code;
            i += 1;
        } else if !code && c == '*' && chars.get(i + 1) == Some(&'*') {
            flush(&mut cur, &mut out, bold, code);
            bold = !bold;
            i += 2;
        } else {
            cur.push(c);
            i += 1;
        }
    }
    flush(&mut cur, &mut out, bold, code);
    out
}

// ---------- 读回来的内容:两家各读各的,排成字在 trace_page ----------

/// 改动的一行。c:'-' 删 / '+' 加 / ' ' 上下文 / '~' 两处改动中间;no:删的写原来的行号,别的写改完的(不知道就没有)
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Change {
    pub c: char,
    pub no: Option<u64>,
    pub text: String,
}

/// 一个工具的内容,两家读回来整理成一样的:右边、详情、e 都按它排
#[derive(Debug, Default)]
pub(crate) struct Io {
    /// 跑的命令(一条或几条)和它的说明(Claude 的 Bash 写的 description)
    pub cmds: Vec<String>,
    pub note: Option<String>,
    /// 改的文件:(路径, 改动)。改文件的工具即使改动是空的也有一项
    pub edits: Vec<(String, Vec<Change>)>,
    /// 读到的文件内容:(路径, 每行的行号和内容)
    pub read: Option<(String, Vec<(u64, String)>)>,
    /// 别的输入:(语言, 整理好的字)
    pub input: Option<(&'static str, String)>,
    pub out: Option<String>,
    pub exit: Option<i64>,
    /// e 打开的真文件:(路径, 第几行)
    pub file: Option<(String, u64)>,
}

/// 一次调用想的、说的(一块一段)
pub(crate) fn said(t: &Trace, i: usize) -> (Vec<String>, Vec<String>) {
    match t.agent {
        Agent::Codex => crate::trace_codex::said(t, &t.nodes[i]),
        _ => crate::trace_claude::said(t, &t.nodes[i]),
    }
}

/// 一轮是谁问的。判断用它,显示时才变成字(字会跟着语言变)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Asker {
    You,
    /// Claude 的 `↩` 那种:后台的事做完了通知它
    Notice,
    /// 子代理收到的、派给它的任务
    Task,
    /// 记录从这一轮中间开始:谁也不是
    Nobody,
}

impl Asker {
    fn label(self) -> &'static str {
        match self {
            Asker::You => Tr::new("你", "You").get(),
            Asker::Notice => Tr::new("通知", "Notice").get(),
            Asker::Task => Tr::new("任务", "Task").get(),
            Asker::Nobody => "",
        }
    }
}

/// 一轮是谁问的、问了什么(记录从一轮中间开始的谁也不是;子代理收到的是派给它的任务)
pub(crate) fn asked(t: &Trace, i: usize) -> (&'static str, String) {
    let (who, text) = match t.agent {
        Agent::Codex => crate::trace_codex::asked(t, &t.nodes[i]),
        _ => crate::trace_claude::asked(t, &t.nodes[i]),
    };
    (if t.is_sub && who == Asker::You { Asker::Task } else { who }.label(), text)
}

pub(crate) fn io(t: &Trace, i: usize) -> Io {
    match t.agent {
        Agent::Codex => crate::trace_codex::io(t, &t.nodes[i]),
        _ => crate::trace_claude::io(t, &t.nodes[i]),
    }
}

/// 出错那条写了什么
pub(crate) fn fail_text(t: &Trace, i: usize) -> String {
    match t.agent {
        Agent::Codex => crate::trace_codex::fail_text(t, &t.nodes[i]),
        _ => crate::trace_claude::fail_text(t, &t.nodes[i]),
    }
}

/// 一次调用的详情里多写的一句(Claude:缓存为什么没命中)
pub(crate) fn call_note(t: &Trace, i: usize) -> Option<String> {
    match t.agent {
        Agent::Codex => None,
        _ => crate::trace_claude::call_note(t, &t.nodes[i]),
    }
}

/// 一个节点的详情(右边那栏 / 整屏的那页)
pub fn detail(t: &Trace, i: usize, live: bool, now: i64) -> Vec<Para> {
    crate::trace_page::detail(t, i, live, now)
}

/// 左边对话里的一轮
pub struct Chat {
    /// 你说的(命令、通知写摘要)
    pub asked: Vec<Para>,
    /// Claude 最后说的(在跑的、没说话的没有)
    pub reply: Vec<Para>,
    /// 一行数字:几点、多久、几次调用、几个工具、钱(在跑 / 出错 / 被打断打头)
    pub foot: Para,
}

pub fn chat(t: &Trace, i: usize, live: bool, now: i64) -> Chat {
    crate::trace_page::chat(t, i, live, now)
}

/// 右边的一块
pub struct Step {
    pub node: usize,
    pub paras: Vec<Para>,
    /// 能 o 展开 / 收起(收着有没露的,或者已经展开了)
    pub fold: bool,
}

/// 右边:一轮的中间过程,一块一个节点(第一块是这一轮的头);open:哪些块展开了
#[cfg(test)]
pub fn steps(t: &Trace, turn: usize, live: bool, now: i64, open: &dyn Fn(usize) -> bool) -> Vec<Step> {
    crate::trace_page::steps(t, turn, live, now, open)
}

/// 右边一块块是哪些节点
pub fn step_nodes(t: &Trace, turn: usize) -> Vec<usize> {
    crate::trace_page::flow_nodes(t, turn)
}

/// 右边的第 k 块排出来取决于的东西(一样就不用重排,见 trace_page::step_sig)
pub fn step_sig(t: &Trace, turn: usize, list: &[usize], k: usize, live: bool, now: i64, open: &dyn Fn(usize) -> bool) -> u64 {
    crate::trace_page::step_sig(t, turn, list, k, live, now, open)
}

/// 右边的第 k 块单独再排一次(看得见了要上色)
pub fn step(t: &Trace, turn: usize, k: usize, live: bool, now: i64, open: &dyn Fn(usize) -> bool) -> Option<Step> {
    crate::trace_page::step(t, turn, k, live, now, open)
}

/// e 用编辑器看的:真的文件(停在第几行),或者一份写成临时文件的字(ext = 临时文件的扩展名,编辑器按它上色)
#[derive(Debug, PartialEq)]
pub enum View {
    File { path: String, line: usize },
    Text { text: String, ext: String, line: usize },
}

pub fn view(t: &Trace, i: usize, live: bool, now: i64) -> Option<View> {
    crate::trace_page::view(t, i, live, now)
}

/// 详情顶上的那行:第几轮 · 第几次调用
pub fn where_of(t: &Trace, i: usize) -> String {
    let n = &t.nodes[i];
    let turn = turn_of(t, i);
    let no = match t.turn_no(turn) {
        Some(x) => tr!("第 {x} 轮", "turn {x}"),
        None if t.nodes[turn].rolled => t!("回退掉的", "rolled back").into(),
        None => t!("接续的", "continued").into(),
    };
    match n.kind {
        Kind::Turn => no,
        Kind::Call | Kind::Fail => {
            let k = n.parent.map_or(0, |p| {
                t.nodes[p].kids.iter().filter(|&&x| t.nodes[x].kind == Kind::Call).position(|&x| x == i).map_or(0, |x| x + 1)
            });
            if n.kind == Kind::Call { tr!("{no} · 第 {k} 次调用", "{no} · call {k}") } else { tr!("{no} · 出错", "{no} · error") }
        }
        Kind::Tool => format!("{} · {no}", tool_name(&n.name)),
        Kind::Mark => n.label.clone(),
        Kind::Copied => t!("接着上一段对话", "continued from the previous conversation").into(),
    }
}

/// 路径写成相对它当时的目录的;家目录写成 ~
pub(crate) fn short_path(p: &str, cwd: &str) -> String {
    if !cwd.is_empty()
        && let Some(rest) = p.strip_prefix(cwd).and_then(|r| r.strip_prefix('/'))
    {
        return rest.to_string();
    }
    let home = std::env::var("HOME").unwrap_or_default();
    match p.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && rest.starts_with('/') => format!("~{rest}"),
        _ => p.to_string(),
    }
}

/// 第一行不空的(去掉首尾空白),最多 limit 个字
pub(crate) fn first_line(s: &str, limit: usize) -> String {
    let l = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    l.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_short() {
        assert_eq!(fmt_dur(8_400), "8.4s");
        assert_eq!(fmt_dur(12_300), "12.3s");
        assert_eq!(fmt_dur(108_000), "1m48s");
        assert_eq!(fmt_dur(9_660_000), "2h41m");
        assert_eq!(fmt_tok(640), "640");
        assert_eq!(fmt_tok(1_204), "1.2k");
        assert_eq!(fmt_tok(92_345), "92k");
        assert_eq!(fmt_tok(1_250_000), "1.2M");
        assert_eq!(fmt_cost(0.052), "$0.05");
        assert_eq!(fmt_cost(123.4), "$123");
        assert_eq!(thousands(88_102), "88,102");
        assert_eq!(thousands(12), "12");
        assert_eq!(clock(0, 8 * 3600, false), "08:00");
        assert_eq!(short_path("/p/app/src/a.rs", "/p/app"), "src/a.rs");
        assert_eq!(first_line("\n  hi there\nnext", 5), "hi th");
    }

    fn texts(ps: &[Para], w: usize) -> Vec<String> {
        wrap(ps, w).iter().map(crate::ansi::plain).collect()
    }

    #[test]
    fn markdown_loses_its_marks_and_keeps_its_shape() {
        let ps = md(
            "## 结论\n改成 **按宽度** 算 `width()`:\n- 第一条说得很长很长很长\n2. 第二步\n> 引用\n```\nlet w = 1;\n```\n| a | b |\n|---|---|",
            Style::new(),
        );
        let t = texts(&ps, 14);
        assert_eq!(
            t,
            [
                "结论",
                "改成 按宽度 算",
                "width():",
                "• 第一条说得很",
                "  长很长很长",
                "2. 第二步",
                "│ 引用",
                "│ let w = 1;",
                "| a | b |",
                "|---|---|"
            ]
        );
        // 粗体是粗体、代码是强调色,记号都去掉了
        let line = &ps[1].body;
        assert!(line.iter().any(|s| s.text == "按宽度" && s.style.add_modifier.contains(Modifier::BOLD)));
        assert!(line.iter().any(|s| s.text == "width()" && s.style.fg == Some(theme::pal().accent)));
    }

    #[test]
    fn wrapped_lines_keep_their_hanging_indent() {
        let ps = hang(md("一二三四五六七八\n第二段", Style::new()), &vec![seg("你  ", Style::new())], &vec![seg("    ", Style::new())]);
        assert_eq!(texts(&ps, 12), ["你  一二三四", "    五六七八", "    第二段"]);
        // 太窄也至少一格一行,不死循环
        assert!(!texts(&ps, 2).is_empty());
    }
}
