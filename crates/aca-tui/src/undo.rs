//! `^Z` 撤销(做过能反悔的都撤):一步一步往回,最多记 `KEEP` 步,只在这个 aca 开着时有效,
//! 不记重做。删会话、删分组撤得回,是因为 `d` 删的进回收站、1 天后才真删(见 trash.rs);挤出这 10 步的照样在回收站里。
//! 重启、发便签、分叉做了就回不去,不记;关掉的窗格 / 普通 tmux 会话进程已经杀了,也记一步(`Gone`),
//! `^Z` 到这只说撤不回 —— 免得越过它去撤更早的那步。

use crate::data::State;
use crate::ops::Target;
use crate::reads::Mark;
use crate::spaces::Group;

/// 最多记这么多步
pub const KEEP: usize = 10;

#[derive(Clone, Debug, PartialEq)]
pub enum Undo {
    /// d 删的登记的会话 / 分组:回收站里的那一条(id),label 是它叫什么;start:删之前在跑的会话,撤回后再启动
    Bin { id: String, label: String, start: Vec<String> },
    /// d 关掉的窗格 / 删掉的普通 tmux 会话:撤不回
    Gone { title: String },
    /// D 停掉的:撤回 = 再启动
    Stop { t: Target },
    /// D 停下的那一格(会话里另开的窗口 / 普通 tmux 会话):撤回 = 在原地再启动(见 park.rs)
    Park { t: Target, p: crate::park::Pane },
    /// m:挪回 from
    Move { t: Target, from: String, groups: Vec<String> },
    /// r:改回 from;unlock:原来的标题没锁(改名会锁上)
    Rename { t: Target, from: String, unlock: bool },
    /// M:反过来挂 / 卸
    Mcp { t: Target, dir: String, on: Vec<String>, off: Vec<String> },
    /// p:原来置顶在第几个(None = 原来没置顶)。撤的时候按 key(`notes::key`)找现在的那一行
    Pin { key: String, name: String, title: String, rank: Option<usize> },
    /// u:已读文件里原来记的、原来的状态;what:当时标成了什么(已读 / 未读);seen:登记的会话标成已读时,核心那份原来记的「看过」的时刻
    Read { key: String, name: String, title: String, what: String, mark: Option<Mark>, state: State, seen: Option<u64> },
    /// Q:工作区文件里这些会话 / 组原来记的
    Space { what: String, sessions: Vec<(String, Option<char>)>, groups: Vec<(Group, Option<char>)> },
    /// e / ^G:原来的便签(空 = 原来没有)
    Note { key: String, title: String, text: String },
    /// 分组上 r:改回去(名字、默认目录),就是反过来再改一次
    Group(crate::groups::Act),
    /// v 多选后一起做的(停、挪分组、置顶、标已读 / 未读):一步全撤回。一起删的在回收站里是一条,记的是 `Bin`
    Many(Vec<Undo>),
}

impl Undo {
    /// 要在后台撤的那几种:撤的是哪个会话
    pub fn target(&self) -> Option<&Target> {
        match self {
            Undo::Stop { t, .. } | Undo::Move { t, .. } | Undo::Rename { t, .. } | Undo::Mcp { t, .. } => Some(t),
            _ => None,
        }
    }
}

/// 最近做的在最后;超过 KEEP 步挤掉最早的
pub fn push(stack: &mut Vec<Undo>, u: Undo) {
    stack.push(u);
    if stack.len() > KEEP {
        stack.remove(0);
    }
}
