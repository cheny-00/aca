//! 计费(`$`)的数据:这台电脑上 Claude / Codex 的每次调用,一律按官方 API 价折成美元 —— 订阅账号也照算,
//! 算的是「这些量按 API 买要多少钱」,不管套餐月费;再加上订阅额度还剩多少。
//!
//! 只读本地文件,不联网、不碰登录凭证:
//! - 用量:账号目录里的对话记录。Claude 是 `projects/**/*.jsonl`(子代理的也在里面),Codex 是 `sessions/**/rollout-*.jsonl`
//! - Claude 额度:statusLine 过 `aca limits` 时按会话记下的(带数据时间,见 limits)
//! - Codex 额度:Codex 自己写在对话记录里的 rate_limits(最近一次用它时的)
//! - 对话归哪个会话:登记表里记的对话 id,加上 hook 报过的每一个(/clear、压缩之后会换,见 `runtime::Run::convs`)
//!
//! 同一次调用会被记好几遍,要去重:
//! - Claude:一条回复按内容块拆成几行,usage 抄在每一行上(最后一行的输出数最全);resume / 分叉把历史整段抄进新文件。
//!   按 message.id + requestId 认,留输出最多的那行
//! - Codex:分叉 / 子代理把父对话的记录连时间戳整段抄过去;额度刷新时把没变的累计数再报一遍。
//!   按(时间, 累计数)认,同一个文件里累计数没变的跳过
//!
//! 记录有好几个 G(几个线程一起读,一两秒),读的时候内存要一百来 MB:界面里不在自己进程读,
//! 而是起一个 `aca ui --cost-json` 子进程读完把结果交回来,读完内存就还了。
//! 读过的存一份在 ~/.cache/aca/cost(见 `Scanner::load`),下回只读新写的

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use memchr::memmem::Finder;
use serde::{Deserialize, Deserializer, Serialize};

use aca_core::i18n::Tr;
use aca_core::{t, tr};

use crate::data::Agent;

// ---------- 价格 ----------

/// 美元 / 百万 token:输入、5 分钟缓存写、1 小时缓存写、缓存读、输出。
/// 抄自 Anthropic、OpenAI 的官方价目表(2026-09-30)。OpenAI 的缓存写只有一档,两格一样;没有缓存写价的填输入价。
/// Claude 4.6 起 100 万上下文也是这个价;Codex 的上下文(25.8 万)到不了 OpenAI 27.2 万以上的长上下文价
const PRICES: &[(&str, [f64; 5])] = &[
    ("claude-fable-5-1", [10.0, 12.5, 20.0, 0.25, 50.0]),
    ("claude-mythos-5-1", [10.0, 12.5, 20.0, 0.25, 50.0]),
    ("claude-fable-5", [10.0, 12.5, 20.0, 1.0, 50.0]),
    ("claude-mythos-5", [10.0, 12.5, 20.0, 1.0, 50.0]),
    ("claude-opus-5-5", [4.0, 5.0, 8.0, 0.2, 20.0]),
    ("claude-opus-5", [5.0, 6.25, 10.0, 0.5, 25.0]),
    ("claude-opus-4-8", [5.0, 6.25, 10.0, 0.5, 25.0]),
    ("claude-opus-4-7", [5.0, 6.25, 10.0, 0.5, 25.0]),
    ("claude-opus-4-6", [5.0, 6.25, 10.0, 0.5, 25.0]),
    ("claude-opus-4-5", [5.0, 6.25, 10.0, 0.5, 25.0]),
    ("claude-opus-4-1", [15.0, 18.75, 30.0, 1.5, 75.0]),
    ("claude-opus-4", [15.0, 18.75, 30.0, 1.5, 75.0]),
    ("claude-sonnet-5-5", [2.0, 2.5, 4.0, 0.2, 10.0]),
    ("claude-sonnet-5", [2.0, 2.5, 4.0, 0.2, 10.0]),
    ("claude-sonnet-4-6", [3.0, 3.75, 6.0, 0.3, 15.0]),
    ("claude-sonnet-4-5", [3.0, 3.75, 6.0, 0.3, 15.0]),
    ("claude-sonnet-4", [3.0, 3.75, 6.0, 0.3, 15.0]),
    ("claude-haiku-4-5", [1.0, 1.25, 2.0, 0.1, 5.0]),
    ("claude-3-5-haiku", [0.8, 1.0, 1.6, 0.08, 4.0]),
    ("gpt-6-astra", [10.0, 12.5, 12.5, 1.0, 50.0]),
    ("gpt-6.1-sol", [2.0, 2.5, 2.5, 0.1, 10.0]),
    ("gpt-6-sol", [2.0, 2.5, 2.5, 0.2, 10.0]),
    ("gpt-6-luna", [0.1, 0.125, 0.125, 0.01, 0.5]),
    ("gpt-5.6-sol", [4.0, 5.0, 5.0, 0.4, 20.0]),
    ("gpt-5.6-terra", [2.0, 2.5, 2.5, 0.2, 12.0]),
    ("gpt-5.6-luna", [0.2, 0.25, 0.25, 0.02, 1.2]),
    ("gpt-5.5", [5.0, 5.0, 5.0, 0.5, 30.0]),
    ("gpt-5.4", [2.5, 2.5, 2.5, 0.25, 15.0]),
    ("gpt-5.3-codex", [1.75, 1.75, 1.75, 0.175, 14.0]),
    ("gpt-5.2", [1.75, 1.75, 1.75, 0.175, 14.0]),
    ("gpt-5.2-codex", [1.75, 1.75, 1.75, 0.175, 14.0]),
    ("gpt-5.1", [1.25, 1.25, 1.25, 0.125, 10.0]),
    ("gpt-5.1-codex", [1.25, 1.25, 1.25, 0.125, 10.0]),
    ("gpt-5.1-codex-max", [1.25, 1.25, 1.25, 0.125, 10.0]),
    ("gpt-5.1-codex-mini", [0.25, 0.25, 0.25, 0.025, 2.0]),
    ("gpt-5", [1.25, 1.25, 1.25, 0.125, 10.0]),
    ("gpt-5-codex", [1.25, 1.25, 1.25, 0.125, 10.0]),
    ("gpt-5-mini", [0.25, 0.25, 0.25, 0.025, 2.0]),
];

/// 联网搜索:每次 1 美分(Claude 的 web_search,另算在 token 之外)
const SEARCH: f64 = 0.01;

/// 记录里的模型名 → 价目表里的名字:去掉 `[1m]` 这类后缀、`-20251001` 这类日期、`azure:` / `openai/` 这类前缀;
/// Claude 的版本号有写成 4.6 的,统一成 4-6
pub fn model_key(m: &str) -> String {
    let m = m.split('[').next().unwrap_or(m).trim();
    let m = m.rsplit(['/', ':']).next().unwrap_or(m).to_ascii_lowercase();
    let m = match m.rsplit_once('-') {
        Some((head, d)) if d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()) => head.to_string(),
        _ => m,
    };
    if m.starts_with("claude-") { m.replace('.', "-") } else { m }
}

pub fn price(model: &str) -> Option<[f64; 5]> {
    let k = model_key(model);
    PRICES.iter().find(|(n, _)| *n == k).map(|(_, p)| *p)
}

/// 快速模式的加价(官方价目表:这三个是两倍,缓存价跟着翻)
fn fast_factor(model: &str) -> f64 {
    match model_key(model).as_str() {
        "claude-opus-5-5" | "claude-opus-5" | "claude-opus-4-8" => 2.0,
        _ => 1.0,
    }
}

/// 一次调用按 API 价多少钱(会话记录 `Z` 用,口径同这里):tok 是输入、5 分钟缓存写、1 小时缓存写、缓存读、输出。
/// 价目表里没有这个模型 = None
pub fn call_cost(model: &str, tok: [u64; 5], fast: bool, us: bool, searches: u64) -> Option<f64> {
    let p = price(model)?;
    let mult = if fast { fast_factor(model) } else { 1.0 } * if us { 1.1 } else { 1.0 };
    let m = |t: u64, per: f64| t as f64 * per / 1e6 * mult;
    Some((0..5).map(|k| m(tok[k], p[k])).sum::<f64>() + searches as f64 * SEARCH)
}

// ---------- 记录 ----------

/// 一次调用
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Rec {
    /// 去重用(见文件头)
    key: u64,
    ts: i64,
    /// 这个文件的第几个模型(Seen.models)
    model: u16,
    /// 输入(不含缓存)、5 分钟缓存写、1 小时缓存写、缓存读、输出
    tok: [u32; 5],
    /// 快速模式、指定只在美国推理(Claude 的加价)
    fast: bool,
    us: bool,
    searches: u16,
}

/// 一个记录文件读到哪、读出了什么
#[derive(Debug, Default)]
struct Seen {
    src: usize,
    agent: Agent,
    /// 上次看到的文件大小;没变就不读
    len: u64,
    /// 读到哪(整行为止,写到一半的那行下次再读)
    off: u64,
    /// 这个文件记的是哪段对话:Claude 的 sessionId(子代理的记在父对话上),Codex 的线程 id(子代理的记在父线程上)
    conv: String,
    conv_set: bool,
    cwd: String,
    models: Vec<String>,
    recs: Vec<Rec>,
    /// key → recs 里的下标(同一个文件里的重复就地合并)。只在读的时候有,读完就扔(省内存,下次从 recs 重建)
    at: HashMap<u64, usize, Fast>,
    /// Codex:最近一次 turn_context 的模型、上一次的累计数、最近一次的额度
    cur: u16,
    total: Option<u64>,
    rate: Option<(i64, XRate)>,
    /// Codex 的子线程 / 分叉出来的线程:开头抄的父线程历史写到这一秒为止(见 codex_line)
    copied_until: i64,
}

impl Seen {
    fn new(src: usize, agent: Agent, path: &Path) -> Self {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        // Codex 的文件名是 rollout-<时间>-<线程 id>
        let conv = if agent == Agent::Codex && stem.len() >= 36 { &stem[stem.len() - 36..] } else { stem };
        Seen { src, agent, conv: conv.to_string(), ..Default::default() }
    }

    fn model(&mut self, name: &str) -> u16 {
        match self.models.iter().position(|m| m == name) {
            Some(i) => i as u16,
            None => {
                self.models.push(name.to_string());
                (self.models.len() - 1) as u16
            }
        }
    }

    /// 同一个 key 留输出最多的那条
    fn push(&mut self, r: Rec) {
        match self.at.entry(r.key) {
            Entry::Occupied(e) => {
                let old = &mut self.recs[*e.get()];
                if r.tok[4] > old.tok[4] {
                    *old = r;
                }
            }
            Entry::Vacant(e) => {
                e.insert(self.recs.len());
                self.recs.push(r);
            }
        }
    }

    fn first_ts(&self) -> i64 {
        self.recs.iter().map(|r| r.ts).min().unwrap_or(i64::MAX)
    }
}

/// 整数当键的表(去重、汇总里几百万次查表)用的哈希:同 rustc 的 FxHash。
/// 默认的 SipHash 是防别人故意造碰撞的,这里的键都是自己算的,用不着,还慢好几倍
#[derive(Default)]
struct IntHash(u64);

impl Hasher for IntHash {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        bytes.iter().for_each(|&b| self.write_u64(b as u64));
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    fn write_i64(&mut self, n: i64) {
        self.write_u64(n as u64);
    }
    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }
}

type Fast = BuildHasherDefault<IntHash>;

fn key_of(parts: &[&str]) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    parts.hash(&mut h);
    h.finish()
}

fn u32s(v: [u64; 5]) -> [u32; 5] {
    v.map(|t| t.min(u32::MAX as u64) as u32)
}

/// null 当 0(有些字段写成 null)
fn num<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Ok(Option::<u64>::deserialize(d)?.unwrap_or(0))
}

fn numf<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.unwrap_or(0.0))
}

#[derive(Deserialize)]
struct CLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "sessionId")]
    session: Option<String>,
    #[serde(rename = "requestId")]
    request: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    message: Option<CMsg>,
}

#[derive(Deserialize)]
struct CMsg {
    id: Option<String>,
    model: Option<String>,
    usage: Option<CUsage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CUsage {
    #[serde(deserialize_with = "num")]
    input_tokens: u64,
    #[serde(deserialize_with = "num")]
    output_tokens: u64,
    #[serde(deserialize_with = "num")]
    cache_read_input_tokens: u64,
    #[serde(deserialize_with = "num")]
    cache_creation_input_tokens: u64,
    cache_creation: Option<CCache>,
    speed: Option<String>,
    inference_geo: Option<String>,
    server_tool_use: Option<CTools>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CCache {
    #[serde(deserialize_with = "num")]
    ephemeral_5m_input_tokens: u64,
    #[serde(deserialize_with = "num")]
    ephemeral_1h_input_tokens: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CTools {
    #[serde(deserialize_with = "num")]
    web_search_requests: u64,
}

static F_USAGE: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"\"usage\""));
static F_ASSISTANT: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"\"assistant\""));
static F_TOKEN_COUNT: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"\"token_count\""));
static F_TURN: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"\"turn_context\""));
static F_META: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"\"session_meta\""));
static F_SETTINGS: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(b"\"thread_settings_applied\""));

/// Claude 的一行:只要带 usage 的回复
fn claude_line(s: &mut Seen, line: &[u8], at: u64) {
    if F_USAGE.find(line).is_none() || F_ASSISTANT.find(line).is_none() {
        return;
    }
    let Ok(l) = serde_json::from_slice::<CLine>(line) else { return };
    if l.kind.as_deref() != Some("assistant") {
        return;
    }
    let Some(m) = l.message else { return };
    let Some(u) = m.usage else { return };
    let model = m.model.unwrap_or_default();
    if model == "<synthetic>" {
        return;
    }
    // 老记录没有分 5 分钟 / 1 小时,算 5 分钟的
    let (w5, w1) = match &u.cache_creation {
        Some(c) if c.ephemeral_5m_input_tokens + c.ephemeral_1h_input_tokens > 0 => {
            (c.ephemeral_5m_input_tokens, c.ephemeral_1h_input_tokens)
        }
        _ => (u.cache_creation_input_tokens, 0),
    };
    let tok = [u.input_tokens, w5, w1, u.cache_read_input_tokens, u.output_tokens];
    let searches = u.server_tool_use.as_ref().map_or(0, |t| t.web_search_requests);
    if tok.iter().all(|&t| t == 0) && searches == 0 {
        return;
    }
    if !s.conv_set {
        s.conv_set = true;
        if let Some(id) = l.session.filter(|x| !x.is_empty()) {
            s.conv = id;
        }
    }
    if s.cwd.is_empty() {
        s.cwd = l.cwd.unwrap_or_default();
    }
    let key = match (m.id.as_deref(), l.request.as_deref()) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => key_of(&["c", a, b]),
        _ => key_of(&["c", s.conv.as_str(), at.to_string().as_str()]),
    };
    let r = Rec {
        key,
        ts: parse_ts(l.timestamp.as_deref().unwrap_or("")),
        model: s.model(&model),
        tok: u32s(tok),
        fast: u.speed.as_deref() == Some("fast"),
        us: u.inference_geo.as_deref() == Some("us"),
        searches: searches.min(u16::MAX as u64) as u16,
    };
    s.push(r);
}

#[derive(Deserialize)]
struct XLine {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<XPayload>,
}

#[derive(Deserialize)]
struct XPayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    parent_thread_id: Option<String>,
    forked_from_id: Option<String>,
    /// "cli" / {"subagent": {"thread_spawn": {"parent_thread_id"}}}
    source: Option<serde_json::Value>,
    history_base: Option<serde_json::Value>,
    cwd: Option<String>,
    model: Option<String>,
    info: Option<XInfo>,
    rate_limits: Option<XRate>,
    thread_settings: Option<XSettings>,
}

#[derive(Deserialize)]
struct XSettings {
    model: Option<String>,
}

/// token_count 的用量(会话记录 `trace_codex` 也照这个读,和这里算得一样)
#[derive(Deserialize)]
pub(crate) struct XInfo {
    pub total_token_usage: Option<XUsage>,
    pub last_token_usage: Option<XUsage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct XUsage {
    #[serde(deserialize_with = "num")]
    pub input_tokens: u64,
    #[serde(deserialize_with = "num")]
    pub cached_input_tokens: u64,
    #[serde(deserialize_with = "num")]
    pub cache_write_input_tokens: u64,
    #[serde(deserialize_with = "num")]
    pub output_tokens: u64,
    #[serde(deserialize_with = "num")]
    pub reasoning_output_tokens: u64,
    #[serde(deserialize_with = "num")]
    pub total_tokens: u64,
}

/// Codex 记下的额度
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct XRate {
    pub primary: Option<XWin>,
    pub secondary: Option<XWin>,
    pub plan_type: Option<String>,
    pub credits: Option<XCredits>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct XWin {
    #[serde(default, deserialize_with = "numf")]
    pub used_percent: f64,
    pub window_minutes: Option<u64>,
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct XCredits {
    #[serde(default)]
    pub has_credits: bool,
    #[serde(default)]
    pub unlimited: bool,
    pub balance: Option<serde_json::Value>,
}

/// Codex 的一行:自己的 session_meta(父线程、目录)、turn_context(模型)、token_count(用量和额度)
fn codex_line(s: &mut Seen, line: &[u8]) {
    let meta = !s.conv_set && F_META.find(line).is_some();
    if !meta && F_TURN.find(line).is_none() && F_TOKEN_COUNT.find(line).is_none() && F_SETTINGS.find(line).is_none() {
        return;
    }
    let Ok(l) = serde_json::from_slice::<XLine>(line) else { return };
    let Some(p) = l.payload else { return };
    match l.kind.as_deref() {
        // 分叉 / 子代理的文件里后面还会抄进父对话的 session_meta,只认第一个
        Some("session_meta") if !s.conv_set => {
            s.conv_set = true;
            let spawn = p
                .source
                .as_ref()
                .and_then(|v| v.pointer("/subagent/thread_spawn/parent_thread_id"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let parent = p.parent_thread_id.filter(|x| !x.is_empty()).or(spawn.filter(|x| !x.is_empty()));
            // 子线程、分叉出来的线程开头整段抄了父线程的历史,连 token_count 一起、时间戳全改成新线程创建的时刻
            // (去重的键对不上父线程的,会再算一遍)。本机 135 份:抄来的都在创建后 0.2 秒内,
            // 它自己的第一次调用最早也在 4.8 秒后 —— 创建后 2 秒内的用量不算。分页分叉(history_base)没抄
            if (parent.is_some() || p.forked_from_id.is_some()) && p.history_base.is_none() {
                s.copied_until = parse_ts(l.timestamp.as_deref().unwrap_or("")) + 2;
            }
            if let Some(parent) = parent {
                s.conv = parent;
            }
            s.cwd = p.cwd.unwrap_or_default();
        }
        Some("turn_context") => {
            if let Some(m) = p.model.filter(|m| !m.is_empty()) {
                learn(s, m);
            }
        }
        // 没有 turn_context 的(子代理之类)模型写在这里,而且常常在第一次 token_count 之后
        Some("event_msg") if p.kind.as_deref() == Some("thread_settings_applied") => {
            if let Some(m) = p.thread_settings.and_then(|t| t.model).filter(|m| !m.is_empty()) {
                learn(s, m);
            }
        }
        Some("event_msg") if p.kind.as_deref() == Some("token_count") => {
            let stamp = l.timestamp.unwrap_or_default();
            let ts = parse_ts(&stamp);
            // 抄来的:额度和用量都是父线程当时的
            if ts <= s.copied_until {
                if let Some(XInfo { total_token_usage: Some(total), .. }) = p.info {
                    s.total = Some(total.total_tokens);
                }
                return;
            }
            if let Some(r) = p.rate_limits
                && s.rate.as_ref().is_none_or(|(t, _)| ts >= *t)
            {
                s.rate = Some((ts, r));
            }
            let Some(XInfo { total_token_usage: Some(total), last_token_usage: Some(last) }) = p.info else { return };
            // 额度刷新时重发的累计数:没有新调用
            if s.total == Some(total.total_tokens) {
                return;
            }
            s.total = Some(total.total_tokens);
            let cached = last.cached_input_tokens.min(last.input_tokens);
            let write = last.cache_write_input_tokens.min(last.input_tokens - cached);
            let tok = [last.input_tokens - cached - write, write, 0, cached, last.output_tokens];
            if tok.iter().all(|&t| t == 0) {
                return;
            }
            if s.models.is_empty() {
                s.cur = s.model("");
            }
            let key = key_of(&["x", stamp.as_str(), total.total_tokens.to_string().as_str(), last.total_tokens.to_string().as_str()]);
            s.push(Rec { key, ts, model: s.cur, tok: u32s(tok), ..Default::default() });
        }
        _ => {}
    }
}

/// Codex 换了模型。之前的调用还不知道是哪个模型(占位的空名字)的话,就是它
fn learn(s: &mut Seen, m: String) {
    match s.models.get(s.cur as usize) {
        Some(cur) if cur.is_empty() => s.models[s.cur as usize] = m,
        _ => s.cur = s.model(&m),
    }
}

/// 从 off 读到文件尾,只处理整行
fn read_file(path: &Path, s: &mut Seen, len: u64) {
    if len < s.off {
        // 变短了 = 被重写过,从头读
        *s = Seen::new(s.src, s.agent, path);
    }
    let Ok(mut f) = File::open(path) else { return };
    // 接着读之前看一眼:上次读到的地方前面该是换行。不是 = 文件换过了(改名顶替、整个重写),从头读
    let mut last = [0u8];
    if s.off > 0 && (f.seek(SeekFrom::Start(s.off - 1)).is_err() || f.read_exact(&mut last).is_err() || last[0] != b'\n') {
        *s = Seen::new(s.src, s.agent, path);
    }
    if f.seek(SeekFrom::Start(s.off)).is_err() {
        return;
    }
    if s.at.is_empty() {
        s.at = s.recs.iter().enumerate().map(|(i, r)| (r.key, i)).collect();
    }
    let mut r = BufReader::with_capacity(1 << 16, f);
    let mut buf = Vec::new();
    loop {
        // 带图片的行能有几十 MB,读过一行大的就把缓冲还回去
        if buf.capacity() > 1 << 20 {
            buf = Vec::new();
        }
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if buf.last() != Some(&b'\n') {
            break;
        }
        let at = s.off;
        s.off += buf.len() as u64;
        if s.agent == Agent::Codex {
            codex_line(s, &buf);
        } else {
            claude_line(s, &buf, at);
        }
    }
    s.len = len;
    s.at = HashMap::default();
    s.recs.shrink_to_fit();
}

// ---------- 账号目录 ----------

/// 一个账号目录
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub agent: Agent,
    /// 账号槽名,对不上就是目录名
    pub who: String,
    pub dir: PathBuf,
}

/// 要读哪些账号目录:config.toml 里登记了配置目录的账号(`accounts`:哪家、名字、目录),再加上两家的默认目录(~/.claude、~/.codex)。
/// 同一个目录只读一次,名字用登记的第一个账号。两个账号的记录目录(projects / sessions)链到同一处的,
/// 分不出是谁的,合成一个、名字连起来(`甲·乙`)
pub fn sources(accounts: &[(Agent, String, String)], home: &str) -> Vec<Source> {
    let mut dirs: Vec<(Agent, String)> = accounts.iter().map(|(a, _, d)| (*a, d.clone())).collect();
    dirs.push((Agent::Claude, format!("{home}/.claude")));
    dirs.push((Agent::Codex, format!("{home}/.codex")));
    let mut out: Vec<Source> = vec![];
    // 每个 out 的(配置目录, 记录目录),都是真实路径
    let mut seen: Vec<(PathBuf, Option<PathBuf>)> = vec![];
    for (agent, d) in dirs {
        let Ok(real) = std::fs::canonicalize(&d) else { continue };
        let data = std::fs::canonicalize(real.join(if agent == Agent::Codex { "sessions" } else { "projects" })).ok();
        let who = accounts
            .iter()
            .find(|s| s.0 == agent && s.2 == d)
            .map(|s| s.1.clone())
            .unwrap_or_else(|| d.rsplit('/').next().unwrap_or(&d).trim_start_matches('.').to_string());
        let same = out.iter().zip(&seen).position(|(o, (r, dt))| o.agent == agent && (*r == real || (data.is_some() && *dt == data)));
        match same {
            Some(i) if seen[i].0 == real => {}
            Some(i) => out[i].who = format!("{}·{who}", out[i].who),
            None => {
                seen.push((real, data));
                out.push(Source { agent, who, dir: PathBuf::from(d) });
            }
        }
    }
    out
}

/// 一个账号目录下的记录文件
fn files_of(src: &Source) -> Vec<PathBuf> {
    let roots: Vec<PathBuf> = match src.agent {
        Agent::Codex => vec![src.dir.join("sessions"), src.dir.join("archived_sessions")],
        _ => vec![src.dir.join("projects")],
    };
    let mut out = vec![];
    let mut todo = roots;
    while let Some(d) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_dir() {
                todo.push(p);
            } else if ft.is_file() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                let ok = name.ends_with(".jsonl") && (src.agent != Agent::Codex || name.starts_with("rollout-"));
                if ok {
                    out.push(p);
                }
            }
        }
    }
    out
}

// ---------- 读 ----------

/// 读过的记录都记在这里。再 scan 只读新写的;界面每次起新的子进程,读过的存在磁盘上(见 `load` / `save`)
#[derive(Default)]
pub struct Scanner {
    srcs: Vec<Source>,
    files: HashMap<PathBuf, Seen>,
    /// 上次 scan 读到了新的 / 丢了文件:要存一份
    dirty: bool,
}

impl Scanner {
    /// 把新写的读进来:新文件从头读,变长的从上次读到的地方接着读,删掉的丢掉。几个线程一起读
    pub fn scan(&mut self, srcs: Vec<Source>) {
        let mut want: HashMap<PathBuf, (usize, Agent)> = HashMap::new();
        for (i, s) in srcs.iter().enumerate() {
            for p in files_of(s) {
                want.entry(p).or_insert((i, s.agent));
            }
        }
        // 账号目录换了(改了配置):从头来
        if srcs != self.srcs {
            self.files.clear();
            self.srcs = srcs;
        }
        let had = self.files.len();
        self.files.retain(|p, _| want.contains_key(p));
        self.dirty = self.files.len() != had;
        for (p, (i, agent)) in &want {
            self.files.entry(p.clone()).or_insert_with(|| Seen::new(*i, *agent, p));
        }
        let todo: Vec<(&PathBuf, &mut Seen, u64)> = self
            .files
            .iter_mut()
            .filter_map(|(p, s)| {
                let len = std::fs::metadata(p).ok()?.len();
                (len != s.len).then_some((p, s, len))
            })
            .collect();
        self.dirty |= !todo.is_empty();
        let queue = Mutex::new(todo.into_iter());
        let n = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
        std::thread::scope(|sc| {
            for _ in 0..n {
                sc.spawn(|| {
                    loop {
                        let next = queue.lock().unwrap().next();
                        let Some((p, s, len)) = next else { break };
                        read_file(p, s, len);
                    }
                });
            }
        });
    }

    /// 各 Codex 账号最近一次记下的额度
    fn codex_rates(&self) -> Vec<(usize, i64, XRate)> {
        let mut best: HashMap<usize, (i64, XRate)> = HashMap::new();
        for s in self.files.values() {
            if let Some((ts, r)) = &s.rate
                && best.get(&s.src).is_none_or(|(t, _)| ts > t)
            {
                best.insert(s.src, (*ts, r.clone()));
            }
        }
        let mut v: Vec<(usize, i64, XRate)> = best.into_iter().map(|(i, (t, r))| (i, t, r)).collect();
        v.sort_by_key(|x| x.0);
        v
    }
}

// ---------- 存一份 ----------

/// 读过的记录存在 ~/.cache/aca/cost(认 XDG_CACHE_HOME)。界面每次按 `$` 起的是新的子进程,
/// 不存的话每次都要把几个 G 的记录从头读一遍;存了就只读新写的(变长的接着读,没变的不碰)。
/// 文件头记着是哪个 aca 写的(程序文件的大小和修改时间):换装了 aca 就从头读一遍,
/// 读法改了(去重、认模型的口径)也不会拿旧版读出来的接着用
const CACHE_MAGIC: &[u8; 8] = b"acacost1";

fn cache_file() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CACHE_HOME").filter(|x| !x.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
    };
    Some(base.join("aca/cost"))
}

/// 这个 aca 程序文件的(大小, 修改时间)
fn exe_stamp() -> Option<(u64, u64)> {
    let m = std::fs::metadata(aca_core::agent::self_path()?).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some((m.len(), t.as_nanos() as u64))
}

#[derive(Default)]
struct Out(Vec<u8>);

impl Out {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u64(b.len() as u64);
        self.0.extend_from_slice(b);
    }
}

struct In<'a>(&'a [u8]);

impl<'a> In<'a> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (a, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*a)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take::<1>().map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take().map(u16::from_le_bytes)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }
    fn u64(&mut self) -> Option<u64> {
        self.take().map(u64::from_le_bytes)
    }
    fn i64(&mut self) -> Option<i64> {
        self.take().map(i64::from_le_bytes)
    }
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = usize::try_from(self.u64()?).ok()?;
        let (a, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(a)
    }
    fn str(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?.to_vec()).ok()
    }
    fn path(&mut self) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(self.bytes()?)))
    }
}

fn path_bytes(p: &Path) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes()
}

const AGENTS: [Agent; 6] = [Agent::None, Agent::Claude, Agent::Codex, Agent::Other, Agent::Pi, Agent::Opencode];

impl Scanner {
    /// 读存着的那份;不是这个 aca 写的、坏了、没有,都当没有(从头读)
    fn load(path: &Path) -> Option<Scanner> {
        Scanner::decode(&std::fs::read(path).ok()?, exe_stamp()?)
    }

    /// 读到了新的才存。先写临时文件再改名(同时开着的 aca 各起一个子进程,各写各的临时文件)
    fn save(&self, path: &Path) {
        let Some(stamp) = exe_stamp().filter(|_| self.dirty) else { return };
        let Some(dir) = path.parent() else { return };
        let _ = std::fs::create_dir_all(dir);
        let tmp = dir.join(format!(".cost.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, self.encode(stamp)).is_err() || std::fs::rename(&tmp, path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    fn encode(&self, stamp: (u64, u64)) -> Vec<u8> {
        let mut o = Out::default();
        o.0.extend_from_slice(CACHE_MAGIC);
        o.u64(stamp.0);
        o.u64(stamp.1);
        o.u32(self.srcs.len() as u32);
        for s in &self.srcs {
            o.u8(AGENTS.iter().position(|a| *a == s.agent).unwrap_or(0) as u8);
            o.bytes(s.who.as_bytes());
            o.bytes(path_bytes(&s.dir));
        }
        o.u32(self.files.len() as u32);
        for (p, s) in &self.files {
            o.bytes(path_bytes(p));
            o.u32(s.src as u32);
            o.u64(s.len);
            o.u64(s.off);
            o.bytes(s.conv.as_bytes());
            o.u8(s.conv_set as u8);
            o.bytes(s.cwd.as_bytes());
            o.u32(s.models.len() as u32);
            for m in &s.models {
                o.bytes(m.as_bytes());
            }
            o.u16(s.cur);
            o.u8(s.total.is_some() as u8);
            o.u64(s.total.unwrap_or(0));
            match &s.rate {
                Some((ts, r)) => {
                    o.u8(1);
                    o.i64(*ts);
                    o.bytes(&serde_json::to_vec(r).unwrap_or_default());
                }
                None => o.u8(0),
            }
            o.i64(s.copied_until);
            o.u32(s.recs.len() as u32);
            for r in &s.recs {
                o.u64(r.key);
                o.i64(r.ts);
                o.u16(r.model);
                r.tok.iter().for_each(|&t| o.u32(t));
                o.u8(r.fast as u8 | (r.us as u8) << 1);
                o.u16(r.searches);
            }
        }
        o.0
    }

    fn decode(data: &[u8], stamp: (u64, u64)) -> Option<Scanner> {
        let mut i = In(data);
        if i.take::<8>()? != *CACHE_MAGIC || (i.u64()?, i.u64()?) != stamp {
            return None;
        }
        let mut sc = Scanner::default();
        for _ in 0..i.u32()? {
            let agent = *AGENTS.get(i.u8()? as usize)?;
            sc.srcs.push(Source { agent, who: i.str()?, dir: i.path()? });
        }
        for _ in 0..i.u32()? {
            let p = i.path()?;
            let src = i.u32()? as usize;
            let mut s = Seen { src, agent: sc.srcs.get(src)?.agent, len: i.u64()?, off: i.u64()?, conv: i.str()?, ..Default::default() };
            s.conv_set = i.u8()? != 0;
            s.cwd = i.str()?;
            for _ in 0..i.u32()? {
                s.models.push(i.str()?);
            }
            s.cur = i.u16()?;
            let (has, total) = (i.u8()? != 0, i.u64()?);
            s.total = has.then_some(total);
            if i.u8()? != 0 {
                let ts = i.i64()?;
                s.rate = Some((ts, serde_json::from_slice(i.bytes()?).ok()?));
            }
            s.copied_until = i.i64()?;
            let n = i.u32()? as usize;
            s.recs.reserve_exact(n.min(i.0.len() / 41));
            for _ in 0..n {
                let (key, ts, model) = (i.u64()?, i.i64()?, i.u16()?);
                let tok = [i.u32()?, i.u32()?, i.u32()?, i.u32()?, i.u32()?];
                let flags = i.u8()?;
                s.recs.push(Rec { key, ts, model, tok, fast: flags & 1 != 0, us: flags & 2 != 0, searches: i.u16()? });
            }
            sc.files.insert(p, s);
        }
        i.0.is_empty().then_some(sc)
    }
}

// ---------- 汇总 ----------

/// 一个额度窗口
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Win {
    /// 5h / 7d
    pub label: String,
    /// 用掉的百分比(超额时会超过 100,照写)
    pub used: f64,
    pub resets: Option<i64>,
}

/// 一个账号的订阅额度
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Quota {
    pub agent: Agent,
    pub who: String,
    /// 套餐名(Codex 记着;Claude 的 statusLine 里没有)
    pub plan: String,
    pub wins: Vec<Win>,
    /// 这份数是什么时候的
    pub updated: i64,
    /// 余额之类的补充
    pub note: String,
}

/// 按什么分的一行
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub name: String,
    /// 灰字补充:账号 / 目录
    pub note: String,
    pub agent: Agent,
    pub cost: f64,
    pub tokens: u64,
    pub calls: u64,
}

/// 一个时段(哪个时段看它在 Report.ranges 里的位置,名字是 RANGES 里的)
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Range {
    pub cost: f64,
    /// 花在哪:输入、缓存写、缓存读、输出、联网搜索
    pub parts: [f64; 5],
    pub tokens: u64,
    /// token 分开算:输入、缓存写、缓存读、输出
    pub toks: [u64; 4],
    pub calls: u64,
    /// 缓存比不用缓存省下的钱:缓存读的折扣减掉缓存写的加价
    pub saved: f64,
    /// 前一个一样长的时段花了多少:昨天 / 再往前 7 天 / 再往前 30 天(全部没有)
    pub prev: f64,
    /// 这个时段有几天(全部从最早一条记录那天算)、其中花了钱的有几天
    pub days: u32,
    pub active: u32,
    /// 今天按小时(0 点到现在),7 / 30 天按天,全部按月(从第一个月到这个月);新的在前
    pub series: Vec<(String, f64)>,
    /// 各账号的走势:一格一格跟 series 对上(新的在前),账号按花费从多到少
    pub src_series: Vec<(String, Agent, Vec<f64>)>,
    /// 星期几(0 = 周日)× 几点,花了多少
    pub heat: [[f64; 24]; 7],
    pub by_src: Vec<Row>,
    pub by_model: Vec<Row>,
    pub by_session: Vec<Row>,
    /// 按对话在哪个目录(家目录写成 ~)
    pub by_dir: Vec<Row>,
    /// 价目表里没有的模型:(模型, token 数),不算进花费
    pub unpriced: Vec<(String, u64)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// 生成的时间和当时的本地时区偏移(秒)
    pub now: i64,
    pub off: i64,
    pub quotas: Vec<Quota>,
    /// 今天、近 7 天、近 30 天、全部
    pub ranges: Vec<Range>,
    /// 最早一条记录
    pub first: Option<i64>,
    /// 只看了哪些账号(空 = 全部;原样是 `--only` 给的)
    #[serde(default)]
    pub only: Vec<String>,
    /// 所有账号一共花了多少(不管只看哪些,都是全部时段),挑账号用
    #[serde(default)]
    pub accounts: Vec<Row>,
    /// 每个账号的颜色位子(见 slots),没有的画灰色
    #[serde(default)]
    pub colors: Vec<(String, usize)>,
    /// 额度那一段顶上要说的一句(开了 aca 的 mod、这一版 Claude 上却没带:为什么);没有就是空的
    #[serde(default)]
    pub quota_note: String,
}

pub const RANGES: [Tr; 4] =
    [Tr::new("今天", "Today"), Tr::new("近 7 天", "7 days"), Tr::new("近 30 天", "30 days"), Tr::new("全部", "All")];

/// 一个账号在面板里叫什么:「Claude 甲」「Codex 乙」(不是 Codex 的都按 Claude 的目录读,也就叫 Claude)
pub fn account_name(agent: Agent, who: &str) -> String {
    format!("{} {who}", if agent == Agent::Codex { "Codex" } else { "Claude" })
}

/// 面板上怎么写一个账号(account_name 起的全名):账号名就是家名的(默认目录 ~/.claude、~/.codex)只写家名
pub fn account_shown(name: &str) -> &str {
    match name.split_once(' ') {
        Some((home, who)) if who.eq_ignore_ascii_case(home) => home,
        _ => name,
    }
}

/// 这个账号在不在只看的里面:空 = 都在;写全名(「Claude 甲」)或账号名都认,链在一起的(`甲·乙`)写哪个都算
pub fn picked(only: &[String], agent: Agent, who: &str) -> bool {
    only.is_empty() || {
        let name = account_name(agent, who);
        only.iter().any(|o| *o == name || *o == who || who.split('·').any(|w| w == o))
    }
}

/// 登记的会话:对话 id → (会话 id, 标题)
#[derive(Default)]
pub struct Deck {
    by_conv: HashMap<String, (String, String)>,
    /// 换过账号接着跑的对话(`aca restart --as`):从什么时候起在哪个配置目录,头一个是换之前的(从最早算起)。
    /// 换之前那几轮两个账号目录里都有一份,一模一样,归当时用的那个
    homes: HashMap<String, Vec<(i64, PathBuf)>>,
}

impl Deck {
    /// 这段对话在 ts 那一刻在哪个配置目录;没换过账号 = None
    fn home_at(&self, conv: &str, ts: i64) -> Option<&Path> {
        let line = self.homes.get(conv)?;
        line.iter().take_while(|(since, _)| *since <= ts).last().map(|(_, h)| h.as_path())
    }
}

/// 比目录用:换了写法、经过链接的也对得上
fn real(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// 读登记表里的会话(归档的也要:对话记录还在)。现在这段对话记在表里,换过的每一段在 hook 写的状态里
pub fn deck() -> Deck {
    let mut d = Deck::default();
    let Ok(store) = aca_core::store::Store::open() else { return d };
    for s in store.sessions(true).unwrap_or_default() {
        let run = aca_core::runtime::load(&s.id);
        let title = aca_core::status::title(&s, &run);
        for c in std::iter::once(&s.conv).chain(&run.convs).chain(std::iter::once(&run.conv)).filter(|c| !c.is_empty()) {
            d.by_conv.entry(c.clone()).or_insert_with(|| (s.id.clone(), title.clone()));
        }
        for m in &run.moves {
            let line = d.homes.entry(m.conv.clone()).or_default();
            if line.is_empty() {
                line.push((i64::MIN, real(Path::new(&m.from))));
            }
            line.push((m.at as i64, real(Path::new(&m.to))));
        }
    }
    d
}

/// 这个 Codex 目录是用 API key 登录的(按量付费,没有订阅额度)
fn api_key_login(dir: &Path) -> bool {
    let Ok(text) = std::fs::read(dir.join("auth.json")) else { return false };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&text) else { return false };
    match v.get("auth_mode").and_then(|m| m.as_str()) {
        Some(mode) => mode == "apikey",
        // 老版本不写 auth_mode:有 key、没有 ChatGPT 的 tokens
        None => v.get("OPENAI_API_KEY").is_some_and(|k| k.is_string()) && v.get("tokens").is_none_or(|t| t.is_null()),
    }
}

/// 几个 Codex 账号的 sessions 链到一处时(见 `sources`)额度记在一起,名字是「甲·乙」;记录里的额度只是其中订阅账号的,
/// 用 API key 登录的那几个名字去掉,一个不剩的不要。accounts 同 `sources`
pub fn subscribed(quotas: Vec<Quota>, accounts: &[(Agent, String, String)], home: &str) -> Vec<Quota> {
    let dir_of = |n: &str| {
        let found = accounts.iter().find(|(a, name, _)| *a == Agent::Codex && name == n);
        found.map_or_else(|| PathBuf::from(format!("{home}/.{n}")), |(_, _, d)| PathBuf::from(d))
    };
    quotas
        .into_iter()
        .filter_map(|mut q| {
            if q.agent == Agent::Codex {
                q.who = q.who.split('·').filter(|n| !api_key_login(&dir_of(n))).collect::<Vec<_>>().join("·");
            }
            (!q.who.is_empty()).then_some(q)
        })
        .collect()
}

fn codex_quota(src: &Source, ts: i64, r: &XRate) -> Option<Quota> {
    let win = |w: &XWin| Win {
        label: match w.window_minutes {
            Some(m) if m > 0 && m % 1440 == 0 => format!("{}d", m / 1440),
            Some(m) if m > 0 && m % 60 == 0 => format!("{}h", m / 60),
            Some(m) => format!("{m}m"),
            None => "?".into(),
        },
        used: w.used_percent,
        resets: w.resets_at,
    };
    let wins: Vec<Win> = [&r.primary, &r.secondary].into_iter().flatten().map(win).collect();
    if wins.is_empty() {
        return None;
    }
    let note = match &r.credits {
        Some(c) if c.unlimited => t!("余额不限", "unlimited balance").to_string(),
        Some(XCredits { has_credits: true, balance: Some(b), .. }) => {
            tr!("余额 {}", "balance {}", b.as_str().map(str::to_string).unwrap_or_else(|| b.to_string()))
        }
        _ => String::new(),
    };
    Some(Quota { agent: Agent::Codex, who: src.who.clone(), plan: r.plan_type.clone().unwrap_or_default(), wins, updated: ts, note })
}

/// 一个 Codex 账号现在的额度,不扫整个目录(换账号的框里用,要快):最近三天里最近改过的几份对话记录,
/// 从末尾往前找最后一次记下的 rate_limits。没有(API key 的账号不记、最近没用过)= None;
/// 最后一次记的是空的(走中转的不带额度)也是 None,不拿更早的旧数顶上 —— 和 `codex_rates` 一样,只认最新的那次
pub fn codex_now(src: &Source) -> Option<Quota> {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = vec![];
    for day in newest_dirs(&src.dir.join("sessions"), 3, 3) {
        for e in std::fs::read_dir(&day).into_iter().flatten().flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                files.push((e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), e.path()));
            }
        }
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    let (ts, r) = files.iter().take(8).find_map(|(_, f)| last_rate(f))?;
    codex_quota(src, ts, &r)
}

/// `dir` 底下 `depth` 层的目录(年 / 月 / 日),名字大的(新的)在前,最多 n 个
fn newest_dirs(dir: &Path, depth: u32, n: usize) -> Vec<PathBuf> {
    let mut subs: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    subs.sort();
    if depth <= 1 {
        return subs.into_iter().rev().take(n).collect();
    }
    let mut out = vec![];
    for d in subs.iter().rev() {
        out.extend(newest_dirs(d, depth - 1, n - out.len()));
        if out.len() >= n {
            break;
        }
    }
    out
}

/// 一份 Codex 记录里最后一次记下的额度和时刻(只读最后 2 MB)
fn last_rate(path: &Path) -> Option<(i64, XRate)> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(2 << 20))).ok()?;
    let mut buf = vec![];
    f.read_to_end(&mut buf).ok()?;
    buf.split(|b| *b == b'\n').rev().filter(|l| Finder::new(b"\"rate_limits\":{").find(l).is_some()).find_map(|l| {
        let v: serde_json::Value = serde_json::from_slice(l).ok()?;
        let r: XRate = serde_json::from_value(v["payload"]["rate_limits"].clone()).ok()?;
        Some((parse_ts(v["timestamp"].as_str()?), r))
    })
}

/// 本地时区偏移(秒):借 SQLite 的 localtime,跟着 TZ 走
pub fn local_offset() -> i64 {
    rusqlite::Connection::open_in_memory()
        .and_then(|c| {
            c.query_row("select cast(strftime('%s','now','localtime') as integer) - cast(strftime('%s','now') as integer)", [], |r| {
                r.get(0)
            })
        })
        .unwrap_or(0)
}

/// 一次调用按 API 价多少钱,分成输入、缓存写、缓存读、输出、搜索
fn cost_parts(r: &Rec, p: &[f64; 5], mult: f64) -> [f64; 5] {
    let m = |t: u32, per: f64| t as f64 * per / 1e6 * mult;
    [m(r.tok[0], p[0]), m(r.tok[1], p[1]) + m(r.tok[2], p[2]), m(r.tok[3], p[3]), m(r.tok[4], p[4]), r.searches as f64 * SEARCH]
}

/// 缓存比不用缓存省了多少:缓存读的那些按输入价要多花的,减掉缓存写比输入价多花的
fn cache_saved(r: &Rec, p: &[f64; 5], mult: f64) -> f64 {
    let m = |t: u32, per: f64| t as f64 * per / 1e6 * mult;
    m(r.tok[3], p[0] - p[3]) - m(r.tok[1], p[1] - p[0]) - m(r.tok[2], p[2] - p[0])
}

/// 家目录换成 ~
fn tilde(dir: &str, home: &str) -> String {
    match dir.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ if dir.is_empty() => "?".into(),
        _ => dir.to_string(),
    }
}

impl Scanner {
    /// 汇总:去重(见文件头)、按时段分、各时段里按账号 / 模型 / 会话 / 目录分
    /// only:只看这些账号(见 `picked`),空 = 全部
    pub fn report(&self, now: i64, off: i64, deck: &Deck, mut quotas: Vec<Quota>, only: &[String]) -> Report {
        for (i, ts, r) in self.codex_rates() {
            if let Some(q) = self.srcs.get(i).and_then(|s| codex_quota(s, ts, &r)) {
                quotas.push(q);
            }
        }
        quotas.retain(|q| picked(only, q.agent, &q.who));
        // 重复的留输出最多的;一样多的留先有的那个文件(分叉出来的文件晚,归到原来的对话上)。
        // 换过账号的对话是例外:两个账号目录里一模一样的那几轮,归当时用的那个账号(见 `Deck::homes`)
        let mut files: Vec<&Seen> = self.files.values().filter(|s| !s.recs.is_empty()).collect();
        files.sort_by_cached_key(|s| (s.first_ts(), s.conv.clone(), s.off));
        let src_dirs: Vec<PathBuf> = if deck.homes.is_empty() { vec![] } else { self.srcs.iter().map(|s| real(&s.dir)).collect() };
        let owns = |s: &Seen, ts: i64| deck.home_at(&s.conv, ts).is_some_and(|h| src_dirs.get(s.src).is_some_and(|d| d == h));
        let mut best: HashMap<u64, (usize, usize), Fast> =
            HashMap::with_capacity_and_hasher(files.iter().map(|s| s.recs.len()).sum(), Fast::default());
        for (fi, s) in files.iter().enumerate() {
            for (ri, r) in s.recs.iter().enumerate() {
                match best.entry(r.key) {
                    Entry::Vacant(e) => {
                        e.insert((fi, ri));
                    }
                    Entry::Occupied(mut e) => {
                        let (f0, r0) = *e.get();
                        let (had, got) = (files[f0].recs[r0].tok[4], r.tok[4]);
                        if got > had || (got == had && !src_dirs.is_empty() && owns(s, r.ts) && !owns(files[f0], r.ts)) {
                            e.insert((fi, ri));
                        }
                    }
                }
            }
        }
        // 对话从哪天开始(子代理的文件晚,按整段对话里最早的算),没登记的对话拿它当名字的一部分
        let mut conv_first: HashMap<&str, i64> = HashMap::new();
        for s in &files {
            let f = conv_first.entry(s.conv.as_str()).or_insert(i64::MAX);
            *f = (*f).min(s.first_ts());
        }
        // 账号 / 模型 / 会话 / 目录先换成编号(见 Keys);每个文件的每个模型的价格也先查好
        let home = std::env::var("HOME").unwrap_or_default();
        let mut t = Tables::default();
        let src_ids: Vec<usize> = self
            .srcs
            .iter()
            .map(|s| {
                let name = account_name(s.agent, &s.who);
                t.src.id(&name, || Row { name: name.clone(), agent: s.agent, ..Default::default() })
            })
            .collect();
        let src_picked: Vec<bool> = self.srcs.iter().map(|s| picked(only, s.agent, &s.who)).collect();
        struct FileKeys {
            /// 每个模型:编号、价格、快速模式的加价
            models: Vec<(usize, Option<[f64; 5]>, f64)>,
            session: usize,
            dir: usize,
        }
        let keys: Vec<FileKeys> = files
            .iter()
            .map(|s| {
                let agent = self.srcs[s.src].agent;
                let models = s
                    .models
                    .iter()
                    .map(|m| {
                        let key = model_key(m);
                        (t.model.id(&key, || Row { name: key.clone(), agent, ..Default::default() }), price(m), fast_factor(m))
                    })
                    .collect();
                let who = &self.srcs[s.src].who;
                let session = match deck.by_conv.get(&s.conv) {
                    Some((k, title)) => t.session.id(k, || Row { name: title.clone(), note: who.clone(), agent, ..Default::default() }),
                    None => {
                        let dir = s.cwd.rsplit('/').find(|x| !x.is_empty()).unwrap_or("?");
                        let name = format!("{dir} · {}", md(conv_first[s.conv.as_str()], off));
                        t.session.id(&format!("c/{}", s.conv), || Row { name, note: who.clone(), agent, ..Default::default() })
                    }
                };
                let d = tilde(&s.cwd, &home);
                let dir = t.dir.id(&d, || Row { name: d.clone(), ..Default::default() });
                FileKeys { models, session, dir }
            })
            .collect();
        let day = |t: i64| (t + off).div_euclid(86400);
        let today = day(now);
        let starts = [today, today - 6, today - 29, i64::MIN];
        // 前一个一样长的时段 [起, 止)
        let before = [(today - 1, today), (today - 13, today - 6), (today - 59, today - 29)];
        let mut acc: Vec<Acc> = RANGES.iter().map(|_| Acc::new(&t)).collect();
        let mut accounts = Acc::new(&t);
        let mut first: Option<i64> = None;
        for &(fi, ri) in best.values() {
            let s = files[fi];
            let r = &s.recs[ri];
            let pick = src_picked[s.src];
            if r.ts > 0 && pick {
                first = Some(first.map_or(r.ts, |f| f.min(r.ts)));
            }
            let d = day(r.ts);
            let hour = ((r.ts + off).rem_euclid(86400) / 3600) as usize;
            let (model, p, fast) = keys[fi].models[r.model as usize];
            let cost = p.map(|p| {
                let mult = if r.fast { fast } else { 1.0 } * if r.us { 1.1 } else { 1.0 };
                (cost_parts(r, &p, mult), cache_saved(r, &p, mult))
            });
            let hit = Hit {
                bucket: 0,
                day: d,
                wd: weekday(d),
                hour,
                model_name: s.models.get(r.model as usize).map_or("", |m| m.as_str()),
                src: src_ids[s.src],
                model,
                session: keys[fi].session,
                dir: keys[fi].dir,
                tok: [r.tok[0] as u64, r.tok[1] as u64 + r.tok[2] as u64, r.tok[3] as u64, r.tok[4] as u64],
                cost,
            };
            // 全部都看的时候,账号列表就是「全部」时段的按账号,不用另算
            if !only.is_empty() {
                accounts.add(&hit);
            }
            if !pick {
                continue;
            }
            for (i, a) in acc.iter_mut().enumerate() {
                if let (Some(&(from, to)), Some((p, _))) = (before.get(i), &hit.cost)
                    && (from..to).contains(&d)
                {
                    a.r.prev += p.iter().sum::<f64>();
                }
                if d < starts[i] {
                    continue;
                }
                let bucket = match i {
                    0 => hour as i64,
                    1 | 2 => d,
                    _ => month(d),
                };
                a.add(&Hit { bucket, ..hit });
            }
        }
        let first_day = first.map(day);
        let ranges: Vec<Range> = acc
            .into_iter()
            .enumerate()
            .map(|(i, a)| a.finish(&t, i, today, ((now + off).rem_euclid(86400) / 3600) as usize, first_day))
            .collect();
        let accounts = if only.is_empty() { ranges[3].by_src.clone() } else { accounts.finish(&t, 3, today, 0, first_day).by_src };
        Report { now, off, quotas, ranges, first, only: only.to_vec(), accounts, colors: vec![], quota_note: String::new() }
    }
}

/// 汇总时的一种分法(账号 / 模型 / 会话 / 目录):名字先换成编号,每次调用按编号累加。
/// 几十万次调用 × 4 个时段,每次都拼名字、按字符串查表的话,光这就要零点几秒
#[derive(Default)]
struct Keys {
    ids: HashMap<String, usize>,
    /// 每个编号那一行的名字、灰字、哪家(数都是 0)
    rows: Vec<Row>,
}

impl Keys {
    fn id(&mut self, key: &str, row: impl FnOnce() -> Row) -> usize {
        if let Some(&i) = self.ids.get(key) {
            return i;
        }
        self.ids.insert(key.to_string(), self.rows.len());
        self.rows.push(row());
        self.rows.len() - 1
    }
}

#[derive(Default)]
struct Tables {
    src: Keys,
    model: Keys,
    session: Keys,
    dir: Keys,
}

/// 一次调用记到一个时段里要的东西
#[derive(Clone, Copy)]
struct Hit<'a> {
    /// 按小时 / 天 / 月的哪一格
    bucket: i64,
    day: i64,
    /// 星期几(0 = 周日)、几点
    wd: usize,
    hour: usize,
    /// 记录里写的模型名(价目表里没有的照它列)
    model_name: &'a str,
    /// 账号、模型、会话、目录的编号(见 Tables)
    src: usize,
    model: usize,
    session: usize,
    dir: usize,
    /// 输入、缓存写、缓存读、输出
    tok: [u64; 4],
    /// (分项花费, 缓存省的);模型不在价目表里就是 None
    cost: Option<([f64; 5], f64)>,
}

/// 按编号累加的一行:花费、token、次数
#[derive(Clone, Copy, Default)]
struct Tally {
    cost: f64,
    tokens: u64,
    calls: u64,
}

impl Tally {
    fn bump(&mut self, cost: f64, tokens: u64) {
        self.cost += cost;
        self.tokens += tokens;
        self.calls += 1;
    }
}

/// 一个时段累加中
struct Acc {
    r: Range,
    series: HashMap<i64, f64, Fast>,
    /// (账号编号, 哪一格) → 花了多少
    src_series: HashMap<(usize, i64), f64, Fast>,
    days: HashSet<i64, Fast>,
    src: Vec<Tally>,
    model: Vec<Tally>,
    session: Vec<Tally>,
    dir: Vec<Tally>,
    unpriced: HashMap<String, u64>,
}

impl Acc {
    fn new(t: &Tables) -> Self {
        let zeros = |k: &Keys| vec![Tally::default(); k.rows.len()];
        Acc {
            r: Range::default(),
            series: HashMap::default(),
            src_series: HashMap::default(),
            days: HashSet::default(),
            src: zeros(&t.src),
            model: zeros(&t.model),
            session: zeros(&t.session),
            dir: zeros(&t.dir),
            unpriced: HashMap::new(),
        }
    }

    fn add(&mut self, h: &Hit) {
        let tokens: u64 = h.tok.iter().sum();
        let Some((parts, saved)) = h.cost else {
            let name = if h.model_name.is_empty() { "?" } else { h.model_name };
            match self.unpriced.get_mut(name) {
                Some(n) => *n += tokens,
                None => {
                    self.unpriced.insert(name.to_string(), tokens);
                }
            }
            return;
        };
        let cost: f64 = parts.iter().sum();
        self.r.cost += cost;
        for (a, b) in self.r.parts.iter_mut().zip(parts) {
            *a += b;
        }
        for (a, b) in self.r.toks.iter_mut().zip(h.tok) {
            *a += b;
        }
        self.r.tokens += tokens;
        self.r.calls += 1;
        self.r.saved += saved;
        self.r.heat[h.wd][h.hour] += cost;
        self.days.insert(h.day);
        *self.series.entry(h.bucket).or_default() += cost;
        *self.src_series.entry((h.src, h.bucket)).or_default() += cost;
        self.src[h.src].bump(cost, tokens);
        self.model[h.model].bump(cost, tokens);
        self.session[h.session].bump(cost, tokens);
        self.dir[h.dir].bump(cost, tokens);
    }

    /// hour = 现在几点;first = 最早一条记录是哪天
    fn finish(self, t: &Tables, i: usize, today: i64, hour: usize, first: Option<i64>) -> Range {
        let mut r = self.r;
        // 记到过的编号换回一行行,花得多的在前;带着编号(按账号的走势要)
        let sorted = |v: Vec<Tally>, k: &Keys| {
            let mut v: Vec<(usize, Row)> = v
                .into_iter()
                .enumerate()
                .filter(|(_, x)| x.calls > 0)
                .map(|(id, x)| (id, Row { cost: x.cost, tokens: x.tokens, calls: x.calls, ..k.rows[id].clone() }))
                .collect();
            v.sort_by(|(_, a), (_, b)| b.cost.total_cmp(&a.cost).then_with(|| a.name.cmp(&b.name)));
            v
        };
        let rows = |v: Vec<(usize, Row)>| v.into_iter().map(|(_, row)| row).collect::<Vec<Row>>();
        let by_src = sorted(self.src, &t.src);
        r.by_model = rows(sorted(self.model, &t.model));
        r.by_session = rows(sorted(self.session, &t.session));
        r.by_dir = rows(sorted(self.dir, &t.dir));
        let mut up: Vec<(String, u64)> = self.unpriced.into_iter().collect();
        up.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        r.unpriced = up;
        let since = first.map_or(1, |f| (today - f + 1).max(1)) as u32;
        r.days = match i {
            0 => 1,
            1 => since.min(7),
            2 => since.min(30),
            _ => since,
        };
        r.active = self.days.len() as u32;
        // 没花钱的小时 / 天 / 月也列上,看得出哪会儿没用
        let keys: Vec<(i64, String)> = match i {
            0 => (0..=hour as i64).rev().map(|h| (h, format!("{h:02}:00"))).collect(),
            1 | 2 => {
                let n = if i == 1 { 7 } else { 30 };
                (0..n).map(|k| today - k).map(|d| (d, day_label(d))).collect()
            }
            _ => match first {
                Some(f) => (month(f)..=month(today)).rev().map(|m| (m, format!("{}-{:02}", m / 12, m % 12 + 1))).collect(),
                None => vec![],
            },
        };
        r.series = keys.iter().map(|(k, l)| (l.clone(), self.series.get(k).copied().unwrap_or(0.0))).collect();
        r.src_series = by_src
            .iter()
            .map(|(id, row)| {
                (row.name.clone(), row.agent, keys.iter().map(|(k, _)| self.src_series.get(&(*id, *k)).copied().unwrap_or(0.0)).collect())
            })
            .collect();
        r.by_src = rows(by_src);
        r
    }
}

/// 同 `build`,读过的记录接着上次存的那份读(见 `Scanner::load`),读完再存回去
pub fn build_cached(only: &[String]) -> Report {
    let path = cache_file();
    let mut sc = path.as_deref().and_then(Scanner::load).unwrap_or_default();
    let r = build(&mut sc, only);
    if let Some(p) = &path {
        sc.save(p);
    }
    r
}

/// 整个流程:读配置 → 读新写的记录 → 汇总
pub fn build(sc: &mut Scanner, only: &[String]) -> Report {
    let home = std::env::var("HOME").unwrap_or_default();
    let accounts = crate::data::accounts(&aca_core::config::load());
    let srcs = sources(&accounts, &home);
    sc.scan(srcs.clone());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    // Claude 的额度是 statusLine 记下的(带数据时间,见 limits)
    let quotas = crate::limits::dir().map(|d| crate::limits::quotas(&d, &srcs, now)).unwrap_or_default();
    let mut r = sc.report(now, local_offset(), &deck(), quotas, only);
    r.quotas = subscribed(std::mem::take(&mut r.quotas), &accounts, &home);
    r.quota_note = crate::limits::mod_note(&aca_core::config::load());
    // 头一回分位子的顺序:近 30 天花得多的先拿(色盘前面几个挨着的颜色最好分),再是更早用过的、只有额度的。
    // 已经有位子的不受这个顺序影响
    let mut names: Vec<String> = vec![];
    let recent = r.ranges.get(2).map(|g| g.by_src.iter().map(|x| x.name.clone()).collect::<Vec<_>>()).unwrap_or_default();
    let quota_names = r.quotas.iter().filter(|q| q.agent != Agent::Other).map(|q| account_name(q.agent, &q.who));
    for n in recent.into_iter().chain(r.accounts.iter().map(|x| x.name.clone())).chain(quota_names) {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    r.colors = crate::slots::assign(crate::state::state_file("colors").as_deref(), &names, crate::theme::pal().series.len());
    r
}

// ---------- 时间 ----------

/// "2026-09-29T02:38:54.270Z" → 秒。认 Z 和 ±hh:mm,认不出就是 0
pub fn parse_ts(s: &str) -> i64 {
    let n = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<i64>().ok());
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(se)) = (n(0, 4), n(5, 7), n(8, 10), n(11, 13), n(14, 16), n(17, 19)) else {
        return 0;
    };
    let mut t = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se;
    if let Some(i) = s.get(19..).and_then(|rest| rest.find(['+', '-'])) {
        let o = &s[19 + i..];
        let sign = if o.starts_with('-') { -1 } else { 1 };
        let (oh, om) = (o.get(1..3).and_then(|x| x.parse::<i64>().ok()), o.get(4..6).and_then(|x| x.parse::<i64>().ok()));
        t -= sign * (oh.unwrap_or(0) * 3600 + om.unwrap_or(0) * 60);
    }
    t
}

/// 公历日期 → 1970-01-01 起第几天
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// 第几天 → (年, 月, 日)
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// 第几天是星期几(0 = 周日)
pub fn weekday(day: i64) -> usize {
    (day + 4).rem_euclid(7) as usize
}

pub const WEEK: [Tr; 7] = [
    Tr::new("周日", "Sun"),
    Tr::new("周一", "Mon"),
    Tr::new("周二", "Tue"),
    Tr::new("周三", "Wed"),
    Tr::new("周四", "Thu"),
    Tr::new("周五", "Fri"),
    Tr::new("周六", "Sat"),
];

/// 从 1970 年 1 月起第几个月
fn month(day: i64) -> i64 {
    let (y, m, _) = civil_from_days(day);
    y * 12 + m - 1
}

/// 9/30 周二
fn day_label(day: i64) -> String {
    let (_, m, d) = civil_from_days(day);
    format!("{m}/{d} {}", WEEK[weekday(day)])
}

/// 本地的 9/30
fn md(ts: i64, off: i64) -> String {
    if ts == i64::MAX || ts <= 0 {
        return "?".into();
    }
    let (_, m, d) = civil_from_days((ts + off).div_euclid(86400));
    format!("{m}/{d}")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `$` 读一份 Claude 记录读出来的每次调用(模型, token):会话记录(trace_claude)的测试拿它对钱
    pub(crate) fn claude_seen(path: &Path) -> Vec<(String, [u64; 5])> {
        let mut s = Seen::new(0, Agent::Claude, path);
        read_file(path, &mut s, std::fs::metadata(path).unwrap().len());
        s.recs.iter().map(|r| (s.models[r.model as usize].clone(), r.tok.map(u64::from))).collect()
    }

    /// `$` 读一份 Codex 记录读出来的每次调用(模型, token):会话记录(trace_codex)的测试拿它对钱
    pub(crate) fn codex_seen(path: &Path) -> Vec<(String, [u64; 5])> {
        let mut s = Seen::new(0, Agent::Codex, path);
        for l in std::fs::read_to_string(path).unwrap().lines() {
            codex_line(&mut s, l.as_bytes());
        }
        s.recs.iter().map(|r| (s.models[r.model as usize].clone(), r.tok.map(u64::from))).collect()
    }

    fn claude(id: &str, req: &str, ts: &str, model: &str, usage: &str) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"s1","requestId":"{req}","timestamp":"{ts}","cwd":"/w/proj","message":{{"id":"{id}","model":"{model}","role":"assistant","content":[{{"type":"text","text":"the \"usage\" word in content"}}],"usage":{usage}}}}}"#
        )
    }

    fn feed(s: &mut Seen, lines: &[String]) {
        for (i, l) in lines.iter().enumerate() {
            if s.agent == Agent::Codex {
                codex_line(s, l.as_bytes());
            } else {
                claude_line(s, l.as_bytes(), i as u64);
            }
        }
    }

    #[test]
    fn model_names_are_normalised_before_looking_up_the_price() {
        assert_eq!(model_key("claude-haiku-4-5-20251001"), "claude-haiku-4-5");
        assert_eq!(model_key("claude-opus-5-5[1m]"), "claude-opus-5-5");
        assert_eq!(model_key("claude-opus-4.6"), "claude-opus-4-6");
        assert_eq!(model_key("proxy/claude-sonnet-4.6"), "claude-sonnet-4-6");
        assert_eq!(model_key("azure:gpt-5.4"), "gpt-5.4");
        assert_eq!(price("gpt-6.1-sol").unwrap()[3], 0.1);
        assert_eq!(price("claude-opus-5").unwrap()[0], 5.0);
        assert_eq!(price("claude-opus-5-5").unwrap()[0], 4.0);
        // 价目表里没有的不猜
        assert_eq!(price("gpt-5.6-sol-ultra"), None);
        assert_eq!(price("gpt-5.5-codex"), None);
        assert_eq!(price("opus"), None);
    }

    #[test]
    fn claude_rows_of_one_reply_count_once_with_the_fullest_output() {
        let u = |out: u64| {
            format!(
                r#"{{"input_tokens":2,"output_tokens":{out},"cache_read_input_tokens":1000,"cache_creation_input_tokens":300,"cache_creation":{{"ephemeral_5m_input_tokens":100,"ephemeral_1h_input_tokens":200}},"speed":"standard","inference_geo":"not_available"}}"#
            )
        };
        let mut s = Seen::new(0, Agent::Claude, Path::new("/p/s1.jsonl"));
        feed(
            &mut s,
            &[
                claude("m1", "r1", "2026-09-29T02:00:00.000Z", "claude-opus-5-5", &u(5)),
                claude("m1", "r1", "2026-09-29T02:00:01.000Z", "claude-opus-5-5", &u(700)),
                claude("m1", "r1", "2026-09-29T02:00:02.000Z", "claude-opus-5-5", &u(700)),
                claude("m2", "r2", "2026-09-29T02:01:00.000Z", "<synthetic>", &u(9)),
                r#"{"type":"user","message":{"role":"user","content":"what is \"usage\"?"}}"#.to_string(),
                "not json {\"usage\" \"assistant\"".to_string(),
            ],
        );
        assert_eq!(s.recs.len(), 1);
        assert_eq!(s.recs[0].tok, [2, 100, 200, 1000, 700]);
        assert_eq!(s.cwd, "/w/proj");
        assert_eq!(s.conv, "s1");
        // 2×4 + 100×5 + 200×8 + 1000×0.2 + 700×20 (每百万)
        let c: f64 = cost_parts(&s.recs[0], &price("claude-opus-5-5").unwrap(), 1.0).iter().sum();
        assert!((c - (8.0 + 500.0 + 1600.0 + 200.0 + 14000.0) / 1e6).abs() < 1e-12);
    }

    #[test]
    fn subagent_files_count_toward_the_conversation_that_started_them() {
        let u = r#"{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":50}"#;
        let mut s = Seen::new(0, Agent::Claude, Path::new("/p/s1/subagents/agent-a1.jsonl"));
        feed(&mut s, &[claude("m9", "r9", "2026-09-29T03:00:00Z", "claude-haiku-4-5-20251001", u)]);
        assert_eq!(s.conv, "s1");
        // 没分 5 分钟 / 1 小时的老记录按 5 分钟算
        assert_eq!(s.recs[0].tok, [1, 50, 0, 0, 1]);
    }

    fn tc(ts: &str, total: u64, last_in: u64, cached: u64, out: u64, rate: &str) -> String {
        let last = last_in + out;
        format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":0,"cached_input_tokens":0,"output_tokens":0,"total_tokens":{total}}},"last_token_usage":{{"input_tokens":{last_in},"cached_input_tokens":{cached},"cache_write_input_tokens":0,"output_tokens":{out},"reasoning_output_tokens":1,"total_tokens":{last}}}}},"rate_limits":{rate}}}}}"#
        )
    }

    const META: &str = r#"{"timestamp":"2026-09-28T00:00:00Z","type":"session_meta","payload":{"id":"t","cwd":"/w/cx","base_instructions":{"text":"long \"token_count\" text"}}}"#;
    const TURN: &str = r#"{"timestamp":"2026-09-28T00:00:01Z","type":"turn_context","payload":{"model":"gpt-6-astra","cwd":"/w/cx"}}"#;
    const RATE: &str = r#"{"limit_id":"codex","primary":{"used_percent":24.0,"window_minutes":10080,"resets_at":1790000000},"secondary":null,"credits":{"has_credits":false,"unlimited":false,"balance":"0"},"plan_type":"plus"}"#;

    #[test]
    fn codex_counts_each_turn_once_and_keeps_the_latest_quota() {
        let path = Path::new("/h/sessions/2026/09/28/rollout-2026-09-28T00-00-00-0199aaaa-bbbb-cccc-dddd-eeeeffff0000.jsonl");
        let mut s = Seen::new(0, Agent::Codex, path);
        assert_eq!(s.conv, "0199aaaa-bbbb-cccc-dddd-eeeeffff0000");
        feed(
            &mut s,
            &[
                META.to_string(),
                TURN.to_string(),
                tc("2026-09-28T00:00:05Z", 1000, 900, 800, 100, "null"),
                // 额度刷新重发的:累计数没变
                tc("2026-09-28T00:00:06Z", 1000, 900, 800, 100, RATE),
                tc("2026-09-28T00:01:00Z", 2500, 1400, 1000, 100, "null"),
            ],
        );
        assert_eq!(s.cwd, "/w/cx");
        assert_eq!(s.recs.len(), 2);
        assert_eq!(s.recs[0].tok, [100, 0, 0, 800, 100]);
        assert_eq!(s.recs[1].tok, [400, 0, 0, 1000, 100]);
        assert_eq!(s.models, vec!["gpt-6-astra".to_string()]);
        let (_, r) = s.rate.clone().unwrap();
        assert_eq!(r.plan_type.as_deref(), Some("plus"));
        let q = codex_quota(&Source { agent: Agent::Codex, who: "cx".into(), dir: PathBuf::new() }, 5, &r).unwrap();
        assert_eq!(q.wins, vec![Win { label: "7d".into(), used: 24.0, resets: Some(1790000000) }]);
        assert_eq!((q.plan.as_str(), q.note.as_str()), ("plus", ""));
    }

    #[test]
    fn a_model_named_after_the_first_turn_still_prices_it() {
        let settings = r#"{"timestamp":"2026-09-28T00:00:07Z","type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"model":"gpt-5.6-sol","cwd":"/w"}}}"#;
        let mut s = Seen::new(0, Agent::Codex, Path::new("/h/sessions/rollout-x.jsonl"));
        feed(
            &mut s,
            &[
                META.to_string(),
                tc("2026-09-28T00:00:05Z", 1000, 900, 800, 100, "null"),
                settings.to_string(),
                tc("2026-09-28T00:01:00Z", 2000, 900, 800, 100, "null"),
                TURN.to_string(),
                tc("2026-09-28T00:02:00Z", 3000, 900, 800, 100, "null"),
            ],
        );
        assert_eq!(s.models, vec!["gpt-5.6-sol".to_string(), "gpt-6-astra".to_string()]);
        assert_eq!(s.recs.iter().map(|r| r.model).collect::<Vec<_>>(), vec![0, 0, 1]);
    }

    #[test]
    fn subagent_threads_roll_up_into_their_parent() {
        let meta =
            r#"{"timestamp":"2026-09-28T00:00:00Z","type":"session_meta","payload":{"id":"k","parent_thread_id":"parent-1","cwd":"/w"}}"#;
        let copied = r#"{"timestamp":"2026-09-27T00:00:00Z","type":"session_meta","payload":{"id":"parent-1","parent_thread_id":"grand","cwd":"/elsewhere"}}"#;
        let mut s = Seen::new(0, Agent::Codex, Path::new("/h/sessions/rollout-x.jsonl"));
        feed(&mut s, &[meta.to_string(), copied.to_string()]);
        assert_eq!((s.conv.as_str(), s.cwd.as_str()), ("parent-1", "/w"));
    }

    #[test]
    fn a_child_thread_does_not_count_the_parent_usage_it_copied_at_birth() {
        let tc = |ts: &str, total: u64, out: u64| {
            format!(
                r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{total},"output_tokens":0,"total_tokens":{total}}},"last_token_usage":{{"input_tokens":100,"cached_input_tokens":0,"output_tokens":{out},"total_tokens":{}}}}},"rate_limits":null}}}}"#,
                100 + out
            )
        };
        let turn = r#"{"timestamp":"2026-09-28T00:00:00.100Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#.to_string();
        // 新版本的子线程:父线程写在 source 里;开头抄的两次用量时间戳和创建时一样,自己的在几秒后
        let meta = r#"{"timestamp":"2026-09-28T00:00:00.000Z","type":"session_meta","payload":{"id":"k","cwd":"/w","source":{"subagent":{"thread_spawn":{"parent_thread_id":"p1","depth":1}}}}}"#;
        let mut s = Seen::new(0, Agent::Codex, Path::new("/h/sessions/rollout-k.jsonl"));
        feed(
            &mut s,
            &[
                meta.to_string(),
                turn.clone(),
                tc("2026-09-28T00:00:00.120Z", 1000, 10),
                tc("2026-09-28T00:00:00.150Z", 2000, 20),
                tc("2026-09-28T00:00:06.000Z", 2300, 30),
            ],
        );
        assert_eq!(s.conv, "p1");
        assert_eq!(s.recs.iter().map(|r| r.tok[4]).collect::<Vec<_>>(), [30]);
        // 分页分叉(history_base)没抄:从头都算
        let meta = r#"{"timestamp":"2026-09-28T00:00:00.000Z","type":"session_meta","payload":{"id":"f","cwd":"/w","forked_from_id":"p1","history_base":{"thread_id":"p1"}}}"#;
        let mut s = Seen::new(0, Agent::Codex, Path::new("/h/sessions/rollout-f.jsonl"));
        feed(&mut s, &[meta.to_string(), turn, tc("2026-09-28T00:00:00.500Z", 110, 10)]);
        assert_eq!((s.conv.as_str(), s.recs.len()), ("rollout-f", 1));
    }

    fn scanner_with(files: Vec<(&str, Seen)>) -> Scanner {
        let srcs = vec![
            Source { agent: Agent::Claude, who: "work".into(), dir: PathBuf::from("/c") },
            Source { agent: Agent::Codex, who: "cx".into(), dir: PathBuf::from("/x") },
        ];
        Scanner { srcs, files: files.into_iter().map(|(p, s)| (PathBuf::from(p), s)).collect(), ..Default::default() }
    }

    #[test]
    fn a_forked_copy_is_counted_once_and_stays_with_the_original() {
        let t = parse_ts("2026-09-30T01:00:00Z");
        let rec = |key: u64, ts: i64, out: u32| Rec { key, ts, model: 0, tok: [0, 0, 0, 0, out], ..Default::default() };
        let mut orig = Seen::new(0, Agent::Claude, Path::new("/c/projects/p/a.jsonl"));
        orig.conv = "a".into();
        orig.cwd = "/w/proj".into();
        orig.models = vec!["claude-opus-5".into()];
        orig.recs = vec![rec(1, t, 1_000_000), rec(2, t + 60, 2_000_000)];
        let mut fork = Seen::new(0, Agent::Claude, Path::new("/c/projects/p/b.jsonl"));
        fork.conv = "b".into();
        fork.models = vec!["claude-opus-5".into()];
        // 抄过来的 1、2,加上自己的 3
        fork.recs = vec![rec(1, t, 1_000_000), rec(2, t + 60, 2_000_000), rec(3, t + 3600, 1_000_000)];
        let mut cx = Seen::new(1, Agent::Codex, Path::new("/x/sessions/rollout-z.jsonl"));
        cx.conv = "z".into();
        cx.models = vec!["gpt-5.5-codex".into(), "gpt-5.4".into()];
        cx.recs = vec![
            Rec { key: 9, ts: t, model: 0, tok: [1000, 0, 0, 0, 0], ..Default::default() },
            Rec { key: 10, ts: t - 3 * 86400, model: 1, tok: [1_000_000, 0, 0, 0, 0], ..Default::default() },
        ];
        cx.rate = Some((
            t,
            XRate {
                primary: Some(XWin { used_percent: 50.0, window_minutes: Some(300), resets_at: Some(t + 3600) }),
                ..Default::default()
            },
        ));
        let sc = scanner_with(vec![("/c/projects/p/a.jsonl", orig), ("/c/projects/p/b.jsonl", fork), ("/x/sessions/rollout-z.jsonl", cx)]);
        let mut deck = Deck::default();
        deck.by_conv.insert("b".into(), ("default/i2".into(), "分出来的".into()));
        let r = sc.report(t + 3600, 8 * 3600, &deck, vec![], &[]);
        let today = &r.ranges[0];
        // 25 + 50 + 25 美元,Codex 那条模型没价格不算
        assert!((today.cost - 100.0).abs() < 1e-9, "{}", today.cost);
        assert_eq!(today.calls, 3);
        assert_eq!(today.unpriced, vec![("gpt-5.5-codex".to_string(), 1000)]);
        let names: Vec<(&str, f64)> = today.by_session.iter().map(|x| (x.name.as_str(), x.cost)).collect();
        assert_eq!(names, vec![("proj · 9/30", 75.0), ("分出来的", 25.0)]);
        assert_eq!(today.by_src[0].name, "Claude work");
        // 三天前那条只在 7 天、30 天、全部里
        assert!((r.ranges[1].cost - 102.5).abs() < 1e-9);
        assert_eq!(r.ranges[1].series.len(), 7);
        assert_eq!(r.ranges[1].series[0].0, "9/30 周三");
        assert_eq!(r.ranges[1].series[3], ("9/27 周日".to_string(), 2.5));
        assert_eq!(r.ranges[3].series, vec![("2026-09".to_string(), 102.5)]);
        // 今天按小时从 0 点列到现在(10 点),没花的也列上
        assert_eq!(today.series.len(), 11);
        assert_eq!(today.series[..3], [("10:00".to_string(), 25.0), ("09:00".to_string(), 75.0), ("08:00".to_string(), 0.0)]);
        assert_eq!((today.heat[3][9], today.heat[3][10]), (75.0, 25.0));
        assert_eq!((today.src_series[0].0.as_str(), &today.src_series[0].2[..3]), ("Claude work", &[25.0, 75.0, 0.0][..]));
        let dirs: Vec<(&str, f64)> = today.by_dir.iter().map(|x| (x.name.as_str(), x.cost)).collect();
        assert_eq!(dirs, vec![("/w/proj", 75.0), ("?", 25.0)]);
        // 最早一条是三天前:近 7 天只有 4 天,其中 2 天花了钱
        assert_eq!((r.ranges[1].days, r.ranges[1].active), (4, 2));
        assert_eq!(r.quotas.len(), 1);
        assert_eq!((r.quotas[0].who.as_str(), r.quotas[0].wins[0].label.as_str()), ("cx", "5h"));
    }

    #[test]
    fn a_codex_accounts_quota_now_comes_from_its_latest_rollouts() {
        let d = std::env::temp_dir().join(format!("aca-codex-now-{}", std::process::id()));
        let day = d.join("sessions/2026/10/14");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::create_dir_all(d.join("sessions/2026/10/13")).unwrap();
        let rate = |used: f64, ts: &str| {
            format!(
                r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","rate_limits":{{"primary":{{"used_percent":{used},"window_minutes":300,"resets_at":1791990000}},"secondary":{{"used_percent":40.0,"window_minutes":10080}},"plan_type":"plus"}}}}}}"#
            )
        };
        // 一份记了两次(取后面那次),一份没记过(API key 那种是 null)
        let a = format!(
            "{}\n{}\n{{\"type\":\"turn_context\",\"payload\":{{}}}}\n",
            rate(10.0, "2026-10-14T01:00:00Z"),
            rate(97.5, "2026-10-14T02:00:00Z")
        );
        std::fs::write(day.join("rollout-2026-10-14T01-00-00-a.jsonl"), a).unwrap();
        std::fs::write(d.join("sessions/2026/10/13/rollout-2026-10-13T01-00-00-b.jsonl"), "{\"payload\":{\"rate_limits\":null}}\n")
            .unwrap();
        let src = Source { agent: Agent::Codex, who: "cx".into(), dir: d.clone() };
        let q = codex_now(&src).unwrap();
        let wins: Vec<(&str, f64)> = q.wins.iter().map(|w| (w.label.as_str(), w.used)).collect();
        assert_eq!((q.who.as_str(), q.plan.as_str(), wins), ("cx", "plus", vec![("5h", 97.5), ("7d", 40.0)]));
        assert_eq!(q.updated, parse_ts("2026-10-14T02:00:00Z"));
        // 后来走中转用的:记了 rate_limits 但里面是空的 —— 空着,不拿上面那份旧数顶
        let relay = day.join("rollout-2026-10-14T03-00-00-c.jsonl");
        std::fs::write(&relay, "{\"timestamp\":\"2026-10-14T03:00:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"rate_limits\":{\"limit_id\":\"codex\",\"primary\":null,\"secondary\":null,\"plan_type\":null}}}\n").unwrap();
        let later = std::fs::metadata(day.join("rollout-2026-10-14T01-00-00-a.jsonl")).unwrap().modified().unwrap()
            + std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(&relay).unwrap().set_modified(later).unwrap();
        assert_eq!(codex_now(&src), None);
        // sessions 链到这里的另一个账号用的是 API key(新老两种 auth.json):额度只是 cx 的
        let quotas =
            |who: &str| vec![Quota { who: who.into(), ..q.clone() }, Quota { agent: Agent::Claude, who: "key".into(), ..q.clone() }];
        let key = d.join("key");
        std::fs::create_dir_all(&key).unwrap();
        std::fs::write(d.join("auth.json"), r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{}}"#).unwrap();
        let accounts = [
            (Agent::Codex, "cx".to_string(), d.to_string_lossy().into_owned()),
            (Agent::Codex, "key".into(), key.to_string_lossy().into_owned()),
        ];
        for auth in [r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-x"}"#, r#"{"OPENAI_API_KEY":"sk-x","tokens":null}"#] {
            std::fs::write(key.join("auth.json"), auth).unwrap();
            let kept = subscribed(quotas("cx·key"), &accounts, "/nowhere");
            assert_eq!(
                kept.iter().map(|q| (q.agent, q.who.as_str())).collect::<Vec<_>>(),
                [(Agent::Codex, "cx"), (Agent::Claude, "key")],
                "{auth}"
            );
            assert_eq!(subscribed(quotas("key"), &accounts, "/nowhere").len(), 1);
        }
        assert_eq!(codex_now(&Source { dir: d.join("nothing"), ..src }), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn turns_copied_to_another_account_stay_with_the_account_they_ran_on() {
        let t = parse_ts("2026-09-30T01:00:00Z");
        let rec = |key: u64, ts: i64| Rec { key, ts, model: 0, tok: [0, 0, 0, 0, 1_000_000], ..Default::default() };
        let seen = |src: usize, path: &str, recs: Vec<Rec>| {
            let mut s = Seen::new(src, Agent::Claude, Path::new(path));
            (s.models, s.recs) = (vec!["claude-opus-5".into()], recs);
            s
        };
        // 在 a 上跑了 1、2;换到 b 跑了 3;又换回 a 跑了 4。a 那份最后被 b 那份盖过,四轮都在,b 那份有前三轮
        let a = seen(0, "/ca/projects/p/c1.jsonl", vec![rec(1, t), rec(2, t + 60), rec(3, t + 3600), rec(4, t + 7200)]);
        let b = seen(1, "/cb/projects/p/c1.jsonl", vec![rec(1, t), rec(2, t + 60), rec(3, t + 3600)]);
        let srcs = vec![
            Source { agent: Agent::Claude, who: "a".into(), dir: PathBuf::from("/ca") },
            Source { agent: Agent::Claude, who: "b".into(), dir: PathBuf::from("/cb") },
        ];
        let files = [("/ca/projects/p/c1.jsonl", a), ("/cb/projects/p/c1.jsonl", b)];
        let sc = Scanner { srcs, files: files.into_iter().map(|(p, s)| (PathBuf::from(p), s)).collect(), ..Default::default() };
        let mut deck = Deck::default();
        deck.homes.insert("c1".into(), vec![(i64::MIN, "/ca".into()), (t + 1800, "/cb".into()), (t + 5400, "/ca".into())]);
        let r = sc.report(t + 7200, 0, &deck, vec![], &[]);
        let by: Vec<(&str, f64)> = r.ranges[3].by_src.iter().map(|x| (x.name.as_str(), x.cost)).collect();
        assert_eq!(by, vec![("Claude a", 75.0), ("Claude b", 25.0)]);
        assert_eq!(r.ranges[3].calls, 4);
    }

    #[test]
    fn a_conversation_is_named_after_the_day_it_started() {
        let t = parse_ts("2026-09-30T01:00:00Z");
        let rec = |key: u64, ts: i64| Rec { key, ts, model: 0, tok: [0, 0, 0, 0, 1_000_000], ..Default::default() };
        let mut main = Seen::new(0, Agent::Claude, Path::new("/c/projects/p/s1.jsonl"));
        (main.conv, main.cwd, main.models, main.recs) =
            ("s1".into(), "/w/proj".into(), vec!["claude-opus-5".into()], vec![rec(1, t - 40 * 86400)]);
        let mut sub = Seen::new(0, Agent::Claude, Path::new("/c/projects/p/s1/subagents/agent-a.jsonl"));
        (sub.conv, sub.cwd, sub.models, sub.recs) = ("s1".into(), "/w/proj".into(), vec!["claude-opus-5".into()], vec![rec(2, t)]);
        let r = scanner_with(vec![("/c/projects/p/s1.jsonl", main), ("/c/projects/p/s1/subagents/agent-a.jsonl", sub)]).report(
            t,
            0,
            &Deck::default(),
            vec![],
            &[],
        );
        // 今天只有子代理那条,名字还是按整段对话开始的那天
        assert_eq!(r.ranges[0].by_session.iter().map(|x| (x.name.as_str(), x.cost)).collect::<Vec<_>>(), vec![("proj · 8/21", 25.0)]);
        assert_eq!(r.ranges[3].by_session.len(), 1);
    }

    #[test]
    fn the_report_survives_the_trip_from_the_child_process() {
        let t = parse_ts("2026-09-30T01:00:00Z");
        let mut s = Seen::new(0, Agent::Claude, Path::new("/c/projects/p/a.jsonl"));
        (s.conv, s.cwd, s.models) = ("a".into(), "/w/p".into(), vec!["claude-opus-5".into()]);
        s.recs = vec![Rec { key: 1, ts: t, tok: [1, 2, 3, 4, 5], ..Default::default() }];
        let q = Quota {
            agent: Agent::Claude,
            who: "a".into(),
            plan: String::new(),
            wins: vec![Win { label: "5h".into(), used: 1.5, resets: None }],
            updated: t,
            note: String::new(),
        };
        let r = scanner_with(vec![("/c/projects/p/a.jsonl", s)]).report(t, 3600, &Deck::default(), vec![q], &[]);
        let back: Report = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn ranges_know_the_period_before_and_what_caching_saved() {
        let t = parse_ts("2026-09-30T01:00:00Z");
        let mut s = Seen::new(0, Agent::Claude, Path::new("/c/projects/p/a.jsonl"));
        (s.conv, s.cwd, s.models) = ("a".into(), "/w/p".into(), vec!["claude-opus-5".into()]);
        // 今天:100 万缓存读 + 10 万 5 分钟缓存写;昨天、10 天前、40 天前各 100 万输出($25)
        s.recs = vec![
            Rec { key: 1, ts: t, tok: [0, 100_000, 0, 1_000_000, 0], ..Default::default() },
            Rec { key: 2, ts: t - 86400, tok: [0, 0, 0, 0, 1_000_000], ..Default::default() },
            Rec { key: 3, ts: t - 10 * 86400, tok: [0, 0, 0, 0, 1_000_000], ..Default::default() },
            Rec { key: 4, ts: t - 40 * 86400, tok: [0, 0, 0, 0, 1_000_000], ..Default::default() },
        ];
        let r = scanner_with(vec![("/c/projects/p/a.jsonl", s)]).report(t, 0, &Deck::default(), vec![], &[]);
        // 缓存读按输入价要多花 100 万 × (5 − 0.5),缓存写比输入价多花 10 万 × (6.25 − 5)
        assert!((r.ranges[0].saved - 4.375).abs() < 1e-9, "{}", r.ranges[0].saved);
        assert_eq!(r.ranges[0].toks, [0, 100_000, 1_000_000, 0]);
        // 昨天、再往前 7 天(10 天前)、再往前 30 天(40 天前)各一条
        assert_eq!(r.ranges.iter().map(|g| g.prev).collect::<Vec<_>>(), vec![25.0, 25.0, 25.0, 0.0]);
        assert_eq!((r.ranges[1].days, r.ranges[1].active), (7, 2));
        assert_eq!((r.ranges[3].days, r.ranges[3].active), (41, 4));
        // 全部按月:从最早那个月(8 月)列到这个月
        assert_eq!(r.ranges[3].series.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec!["2026-09", "2026-08"]);
        // 昨天是周二,凌晨 1 点
        assert_eq!(r.ranges[1].heat[2][1], 25.0);
        assert_eq!(tilde("/home/u/w", "/home/u"), "~/w");
        assert_eq!(tilde("/home/u2/w", "/home/u"), "/home/u2/w");
        assert_eq!(tilde("", "/home/u"), "?");
    }

    #[test]
    fn only_some_accounts_leaves_the_rest_out_but_still_lists_them() {
        let t = parse_ts("2026-09-30T01:00:00Z");
        let mut c = Seen::new(0, Agent::Claude, Path::new("/c/projects/p/a.jsonl"));
        (c.conv, c.cwd, c.models) = ("a".into(), "/w/p".into(), vec!["claude-opus-5".into()]);
        c.recs = vec![Rec { key: 1, ts: t - 20 * 86400, tok: [0, 0, 0, 0, 1_000_000], ..Default::default() }];
        let mut x = Seen::new(1, Agent::Codex, Path::new("/x/sessions/rollout-z.jsonl"));
        (x.conv, x.cwd, x.models) = ("z".into(), "/w/q".into(), vec!["gpt-5.4".into()]);
        x.recs = vec![Rec { key: 2, ts: t, tok: [1_000_000, 0, 0, 0, 0], ..Default::default() }];
        x.rate = Some((
            t,
            XRate { primary: Some(XWin { used_percent: 50.0, window_minutes: Some(300), resets_at: None }), ..Default::default() },
        ));
        let sc = scanner_with(vec![("/c/projects/p/a.jsonl", c), ("/x/sessions/rollout-z.jsonl", x)]);
        let claude_q =
            Quota { agent: Agent::Claude, who: "work".into(), plan: String::new(), wins: vec![], updated: t, note: String::new() };
        let all = sc.report(t, 0, &Deck::default(), vec![claude_q.clone()], &[]);
        let cx = sc.report(t, 0, &Deck::default(), vec![claude_q], &["cx".into()]);
        assert_eq!(cx.only, vec!["cx".to_string()]);
        assert_eq!(cx.ranges[3].by_src.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["Codex cx"]);
        assert_eq!(cx.ranges[3].by_session.len(), 1);
        assert_eq!(cx.quotas.iter().map(|q| q.who.as_str()).collect::<Vec<_>>(), vec!["cx"]);
        // 天数从只看的账号最早那条算
        assert_eq!(cx.ranges[3].days, 1);
        assert_eq!(all.ranges[3].days, 21);
        // 挑账号用的列表不管只看哪个,都是全部账号
        assert_eq!(cx.accounts, all.accounts);
        assert_eq!(all.accounts.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["Claude work", "Codex cx"]);
        assert_eq!(sc.report(t, 0, &Deck::default(), vec![], &["Claude work".into()]).ranges[3].cost, all.accounts[0].cost);
        // 链在一起的账号写哪个名字都认,全名要连着是哪家
        assert!(picked(&["b".into()], Agent::Codex, "a·b") && picked(&["Codex a·b".into()], Agent::Codex, "a·b"));
        assert!(!picked(&["Claude a·b".into()], Agent::Codex, "a·b") && !picked(&["a·".into()], Agent::Codex, "a·b"));
    }

    #[test]
    fn fast_mode_and_us_only_inference_cost_more() {
        let t = parse_ts("2026-09-30T01:00:00Z");
        let mut s = Seen::new(0, Agent::Claude, Path::new("/c/projects/p/a.jsonl"));
        s.models = vec!["claude-opus-5-5".into(), "claude-haiku-4-5".into()];
        s.recs = vec![
            Rec { key: 1, ts: t, model: 0, tok: [1_000_000, 0, 0, 0, 0], fast: true, ..Default::default() },
            Rec { key: 2, ts: t, model: 1, tok: [1_000_000, 0, 0, 0, 0], fast: true, us: true, searches: 3 },
        ];
        let r = scanner_with(vec![("/c/projects/p/a.jsonl", s)]).report(t, 0, &Deck::default(), vec![], &[]);
        // 4×2 + 1×1.1 + 3 次搜索
        assert!((r.ranges[0].cost - (8.0 + 1.1 + 0.03)).abs() < 1e-9, "{}", r.ranges[0].cost);
    }

    #[test]
    fn timestamps_and_calendar() {
        assert_eq!(parse_ts("1970-01-01T00:00:00Z"), 0);
        assert_eq!(parse_ts("2026-09-29T02:38:54.270Z"), 1790649534);
        assert_eq!(parse_ts("2026-09-29T10:38:54+08:00"), 1790649534);
        assert_eq!(parse_ts("garbage"), 0);
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(WEEK[weekday(days_from_civil(2026, 9, 30))].get(), "周三");
        assert_eq!(month(days_from_civil(2026, 1, 15)) % 12, 0);
    }

    #[test]
    fn sources_name_dirs_by_their_slot_and_read_each_dir_once() {
        let home = std::env::temp_dir().join(format!("aca-src-{}", std::process::id()));
        for d in [".claude-a", ".claude", ".codex-b", ".codex/sessions", ".codex-c"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        // c 的记录目录链到 ~/.codex 的:分不出是谁的
        std::os::unix::fs::symlink(home.join(".codex/sessions"), home.join(".codex-c/sessions")).unwrap();
        let h = home.to_string_lossy().to_string();
        let acct = |a: Agent, name: &str, dir: &str| (a, name.to_string(), format!("{h}/{dir}"));
        // 目录不在的(nope)不读;同一个目录登记了两次(a、again)只读一次,用先登记的名字
        let accounts = [
            acct(Agent::Claude, "a", ".claude-a"),
            acct(Agent::Claude, "again", ".claude-a"),
            acct(Agent::Claude, "nope", ".nope"),
            acct(Agent::Codex, "b", ".codex-b"),
            acct(Agent::Codex, "c", ".codex-c"),
        ];
        let got: Vec<(Agent, String)> = sources(&accounts, &h).into_iter().map(|s| (s.agent, s.who)).collect();
        std::fs::remove_dir_all(&home).unwrap();
        assert_eq!(
            got,
            vec![
                (Agent::Claude, "a".into()),
                (Agent::Codex, "b".into()),
                (Agent::Codex, "c·codex".into()),
                (Agent::Claude, "claude".into())
            ]
        );
    }

    #[test]
    fn a_conversation_takes_its_name_from_the_file_that_started_it() {
        let t = parse_ts("2026-09-30T01:00:00Z");
        let rec = |key: u64, ts: i64| Rec { key, ts, model: 0, tok: [0, 0, 0, 0, 1_000_000], ..Default::default() };
        let mut main = Seen::new(0, Agent::Claude, Path::new("/c/projects/p/s1.jsonl"));
        (main.conv, main.cwd, main.models, main.recs) =
            ("s1".into(), "/w/proj".into(), vec!["claude-opus-5".into()], vec![rec(1, t - 86400)]);
        // 子代理在别的目录里干活(worktree 之类),今天只有它那条
        let mut sub = Seen::new(0, Agent::Claude, Path::new("/c/projects/q/s1/subagents/agent-a.jsonl"));
        (sub.conv, sub.cwd, sub.models, sub.recs) = ("s1".into(), "/w/other".into(), vec!["claude-opus-5".into()], vec![rec(2, t)]);
        let r = scanner_with(vec![("/c/projects/p/s1.jsonl", main), ("/c/projects/q/s1/subagents/agent-a.jsonl", sub)]).report(
            t,
            0,
            &Deck::default(),
            vec![],
            &[],
        );
        for g in [0, 3] {
            assert_eq!(r.ranges[g].by_session.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), vec!["proj · 9/29"]);
        }
    }

    #[test]
    fn what_was_read_is_kept_on_disk_and_read_back_as_it_was() {
        let u = r#"{"input_tokens":3,"output_tokens":7,"cache_read_input_tokens":11,"speed":"fast","inference_geo":"us","server_tool_use":{"web_search_requests":2}}"#;
        let mut cl = Seen::new(0, Agent::Claude, Path::new("/c/projects/p/s1.jsonl"));
        feed(
            &mut cl,
            &[
                claude("m1", "r1", "2026-09-29T02:00:00Z", "claude-opus-5-5", u),
                claude("m2", "r2", "2026-09-29T02:01:00Z", "claude-sonnet-4-6", u),
            ],
        );
        (cl.len, cl.off) = (900, 880);
        let cx_path = "/x/sessions/2026/09/28/rollout-2026-09-28T00-00-00-0199aaaa-bbbb-cccc-dddd-eeeeffff0000.jsonl";
        let mut cx = Seen::new(1, Agent::Codex, Path::new(cx_path));
        feed(&mut cx, &[META.to_string(), TURN.to_string(), tc("2026-09-28T00:00:05Z", 1000, 900, 800, 100, RATE)]);
        cx.copied_until = 42;
        for s in [&mut cl, &mut cx] {
            s.at = HashMap::default();
        }
        let srcs = vec![
            Source { agent: Agent::Claude, who: "甲".into(), dir: "/c".into() },
            Source { agent: Agent::Codex, who: "cx".into(), dir: "/x".into() },
        ];
        let files = [("/c/projects/p/s1.jsonl", cl), (cx_path, cx)].into_iter().map(|(p, s)| (PathBuf::from(p), s)).collect();
        let sc = Scanner { srcs, files, dirty: true };
        let bytes = sc.encode((7, 8));
        let back = Scanner::decode(&bytes, (7, 8)).unwrap();
        assert_eq!(back.srcs, sc.srcs);
        assert_eq!(back.files.len(), 2);
        for (p, s) in &sc.files {
            assert_eq!(format!("{:?}", back.files[p]), format!("{s:?}"));
        }
        let t = parse_ts("2026-09-30T00:00:00Z");
        assert_eq!(back.report(t, 0, &Deck::default(), vec![], &[]), sc.report(t, 0, &Deck::default(), vec![], &[]));
        // 别的 aca 写的、写到一半的、后面多了东西的:都当没有,从头读
        assert!(Scanner::decode(&bytes, (7, 9)).is_none());
        assert!(Scanner::decode(&bytes[..bytes.len() - 1], (7, 8)).is_none());
        assert!(Scanner::decode(&[&bytes[..], b"x"].concat(), (7, 8)).is_none());
        assert!(Scanner::decode(b"", (7, 8)).is_none());
    }

    #[test]
    fn a_file_swapped_for_another_is_read_again_from_the_top() {
        let dir = std::env::temp_dir().join(format!("aca-swap-{}", std::process::id()));
        let p = dir.join("projects/x/s1.jsonl");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let u = r#"{"input_tokens":1,"output_tokens":2}"#;
        let line = |id: &str| claude(id, "r", "2026-09-29T02:00:00Z", "claude-opus-5", u);
        std::fs::write(&p, format!("{}\n", line("m1"))).unwrap();
        let src = Source { agent: Agent::Claude, who: "a".into(), dir: dir.clone() };
        let mut sc = Scanner::default();
        sc.scan(vec![src.clone()]);
        assert!(sc.dirty);
        // 换成了另一份更长的:上次读到的地方落在一行中间
        std::fs::write(&p, format!("{}\n{}\n", line("m2-longer"), line("m3"))).unwrap();
        sc.scan(vec![src.clone()]);
        let keys: HashSet<u64> = sc.files[&p].recs.iter().map(|r| r.key).collect();
        assert_eq!(keys, ["m2-longer", "m3"].map(|id| key_of(&["c", id, "r"])).into());
        // 什么都没变:不用存
        sc.scan(vec![src]);
        assert!(!sc.dirty);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn appended_lines_are_read_once_and_half_written_lines_wait() {
        let dir = std::env::temp_dir().join(format!("aca-read-{}", std::process::id()));
        let p = dir.join("projects/x/s1.jsonl");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let u = r#"{"input_tokens":1,"output_tokens":2}"#;
        let l1 = claude("m1", "r1", "2026-09-29T02:00:00Z", "claude-opus-5", u);
        let l2 = claude("m2", "r2", "2026-09-29T02:00:00Z", "claude-opus-5", u);
        std::fs::write(&p, format!("{l1}\n{}", &l2[..20])).unwrap();
        let src = Source { agent: Agent::Claude, who: "a".into(), dir: dir.clone() };
        let mut sc = Scanner::default();
        sc.scan(vec![src.clone()]);
        assert_eq!(sc.files[&p].recs.len(), 1);
        std::fs::write(&p, format!("{l1}\n{l2}\n")).unwrap();
        sc.scan(vec![src.clone()]);
        assert_eq!(sc.files[&p].recs.len(), 2);
        sc.scan(vec![src]);
        assert_eq!(sc.files[&p].recs.len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
