//! 一个会话的运行时状态:`~/.local/state/aca/run/<id>.json`。hook 每个事件来了改它,列表、命令行读它。
//!
//! 一个会话一个文件,不进登记表:hook 写的时候不用等别的会话,也不会被界面查库挡住。
//! 改 = 拿这个会话的锁 → 读 → 改 → 原子写回,所以同一个会话的两个事件前后脚到也不会丢一个。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::paths;

/// 插件挂在会话行上的一小段字(`aca meta set`),列表行里按名字取
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Token {
    pub value: String,
    /// 过了这个时刻不再显示;0 = 一直留着
    #[serde(default, skip_serializing_if = "is_zero")]
    pub until: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Run {
    /// hook 说的状态:running / waiting / done / idle / error / ended;空 = 还没收到过事件
    pub status: String,
    /// 最后一个事件和它的时刻
    pub event: String,
    pub ts: u64,
    /// 现在这段对话的 id(/clear、压缩之后会换),和换过的每一个(计费把用量归到会话用)
    pub conv: String,
    pub convs: Vec<String>,
    pub transcript: String,
    pub cwd: String,
    /// 在等你时它问的那句
    pub ask: String,
    /// 问的是哪种:choice(选择题)/ permission(要不要让它跑)/ 空 = 看不出(只在屏幕上认出来的、启动时那几道关)
    pub ask_kind: String,
    /// 选择题的选项(只记第一题的);`ask_multi` = 一次问了好几题,或者能多选:按一个数字答不了
    pub ask_opts: Vec<String>,
    pub ask_multi: bool,
    /// 这一轮你说的那句(截断)
    pub prompt: String,
    /// 最后一条回复(截断):`c` 复制、通知、插件都从这拿,不用再去翻对话记录
    pub last: String,
    /// 这一轮结束时还挂着的后台任务数
    pub bg: u32,
    /// 这一轮开始 / 最近一次跑完的时刻
    pub turn: u64,
    pub done: u64,
    /// 看过的时刻:跑完在这之后的算「没看」
    pub seen: u64,
    /// agent 自己起的标题;`locked` = 人改过名,不再跟
    pub auto_title: String,
    /// hook 里报上来的标题(协议的 `title`)。报不报、`auto_title` 跟不跟它,看各家的 `Agent::title`
    pub hook_title: String,
    pub locked: bool,
    /// 上次交给 Claude 当它自己名字的(`aca hook-name`):它还叫这个、你又在 aca 里改了名,就跟过去
    pub named: String,
    pub error: String,
    /// 这一轮失败是哪一类:transient(过一会儿接着说就好)/ account(额度、登录,要人处理)/ fatal
    /// (见 [`crate::agent::lifecycle::error_kind`])
    pub error_kind: String,
    /// 分叉出来的会话:从哪段对话分的。分出来还没说过话就停了的,自己没有对话记录,再起时得重新分一次
    pub fork_from: String,
    /// 门铃接上了:这次起来时 SessionStart 让它盯着自己的门铃文件(`aca inbox watch`)。每次启动先放下,接上了再立起来
    pub bell: bool,
    /// 哪些事通知 parent(done,failed,exited,waiting 里的几个,逗号隔开;none = 都不);空 = 照配置的
    pub notify: String,
    pub tokens: BTreeMap<String, Token>,
    /// 这次起来时插件给它带了什么(见 `plugin::mount::signature`):和现在的一比,不一样就是要重启才带上
    pub mounts: String,
    /// 这次 agent 是什么时候起的(`session::launch`):刚起的那一会儿,启动时的关有人替你答(见 [`crate::trust`])
    #[serde(skip_serializing_if = "is_zero")]
    pub started: u64,
}

impl Run {
    /// 还没过期的那些
    pub fn live_tokens(&self, now: u64) -> BTreeMap<String, String> {
        self.tokens.iter().filter(|(_, t)| t.until == 0 || t.until > now).map(|(k, t)| (k.clone(), t.value.clone())).collect()
    }

    /// 不在等你了:问的那句和选项一起清掉
    pub fn clear_ask(&mut self) {
        self.ask.clear();
        self.ask_kind.clear();
        self.ask_opts.clear();
        self.ask_multi = false;
    }

    /// 不算出错了:原文和类别一起清掉
    pub fn clear_error(&mut self) {
        self.error.clear();
        self.error_kind.clear();
    }

    /// 出错的类别给人看的那几个字(没有类别是空的)
    pub fn error_kind_said(&self) -> &'static str {
        match self.error_kind.as_str() {
            "transient" => crate::t!(
                "临时的:限流、过载或断流,过一会儿让它接着干多半就好",
                "transient: rate limit, overload or a dropped stream; letting it carry on in a while usually works"
            ),
            "account" => crate::t!("账号的:额度、登录或欠费,要人处理", "account: quota, login or billing; needs a person"),
            "fatal" => {
                crate::t!("再试也一样:上下文、请求或内容策略的问题", "fatal: context, request or content policy; retrying won't help")
            }
            _ => "",
        }
    }

    pub fn note_conv(&mut self, conv: &str) {
        if conv.is_empty() {
            return;
        }
        if !self.convs.iter().any(|c| c == conv) {
            self.convs.push(conv.to_string());
        }
        self.conv = conv.to_string();
    }
}

pub fn load(id: &str) -> Run {
    std::fs::read_to_string(paths::run_file(id)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// 拿锁、读、改、写回;返回 (改之前的, 改之后的)
pub fn update(id: &str, f: impl FnOnce(&mut Run)) -> (Run, Run) {
    let path = paths::run_file(id);
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    // 锁在旁边一个不动的文件上:状态文件自己每次都是换一个新的进来
    let lock = std::fs::OpenOptions::new().create(true).append(true).open(path.with_extension("lock"));
    if let Ok(l) = &lock {
        let _ = l.lock();
    }
    let before = load(id);
    let mut after = before.clone();
    f(&mut after);
    if after != before {
        let _ = paths::write_atomic(&path, &serde_json::to_string(&after).unwrap_or_default());
    }
    (before, after)
}

pub fn remove(id: &str) {
    let path = paths::run_file(id);
    let _ = std::fs::remove_file(path.with_extension("lock"));
    let _ = std::fs::remove_file(path);
}

/// 留开头的 n 个字
pub fn clip(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_conversation_is_remembered_once_and_becomes_current() {
        let mut r = Run::default();
        r.note_conv("a");
        r.note_conv("b");
        r.note_conv("a");
        r.note_conv("");
        assert_eq!((r.conv.as_str(), r.convs.len()), ("a", 2));
    }

    #[test]
    fn expired_tokens_are_not_shown() {
        let mut r = Run::default();
        r.tokens.insert("pr".into(), Token { value: "#12".into(), until: 0 });
        r.tokens.insert("ci".into(), Token { value: "红".into(), until: 100 });
        assert_eq!(r.live_tokens(99).len(), 2);
        assert_eq!(r.live_tokens(100).keys().collect::<Vec<_>>(), ["pr"]);
    }

    #[test]
    fn clipping_counts_characters_not_bytes() {
        assert_eq!(clip("修登录bug", 3), "修登录…");
        assert_eq!(clip("ok", 3), "ok");
    }

    #[test]
    fn an_old_file_without_newer_fields_still_reads() {
        let r: Run = serde_json::from_str(r#"{"status":"done","conv":"x"}"#).unwrap();
        assert_eq!((r.status.as_str(), r.seen, r.tokens.len()), ("done", 0, 0));
    }
}
