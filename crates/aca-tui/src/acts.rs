//! The main list's keys in one table, by section: key, name, which kind of row it works on. The `?` sheet lists them from here
//! (pressing a row's key there = pressing it on the list, see sheet.rs). A few rows only say (several keys in one, keys pressed in
//! tmux or while making a new session): their key isn't one the sheet can press.
//! Names are short: one per row, and a long one squeezes out the plugin name on the right.

use aca_core::i18n::{Lang, lang};

/// A key: section (see `sections`), key as the bottom bar writes it (`n`, `^N`, `Tab`, `⇧↑`, 「空格」), Chinese and English names,
/// which kind of row it works on (empty = any; "session" a registered session's row, "group" a group row)
pub struct Act {
    pub sec: usize,
    pub key: &'static str,
    zh: &'static str,
    en: &'static str,
    pub on: &'static [&'static str],
}

impl Act {
    pub fn name(&self) -> &'static str {
        if lang() == Lang::En { self.en } else { self.zh }
    }

    /// The key as the sheet shows it: the few written as words get the English word in English
    pub fn label(&self) -> &'static str {
        match self.key {
            "空格" => aca_core::t!("空格", "Space"),
            "数字" => aca_core::t!("数字", "digits"),
            "轻点" => aca_core::t!("轻点", "tap"),
            "前缀 a" => aca_core::t!("前缀 a", "prefix a"),
            k => k,
        }
    }

    /// Usable when the highlighted row is on (session / group / global)
    pub fn fits(&self, on: &str) -> bool {
        self.on.is_empty() || self.on.contains(&on)
    }
}

const fn act(sec: usize, key: &'static str, zh: &'static str, en: &'static str, on: &'static [&'static str]) -> Act {
    Act { sec, key, zh, en, on }
}

const S: &[&str] = &["session"];
const SG: &[&str] = &["session", "group"];
/// "desk": the rail's row for all groups, where the keys that reach the desk work as they do for a group's router
const SGD: &[&str] = &["session", "group", "desk"];

/// The sections' names, in order
pub fn sections() -> [&'static str; 7] {
    if lang() == Lang::En {
        ["Navigate", "Open", "Which to list", "Sessions", "Pages", "In tmux", "When creating"]
    } else {
        ["导航", "打开", "列出哪些", "会话", "页面", "tmux 中", "新建时"]
    }
}

pub const ACTS: &[Act] = &[
    act(0, "↑↓ jk", "上下", "up / down", &[]),
    act(0, "J", "下一组", "next group", &[]),
    act(0, "K", "上一组", "previous group", &[]),
    act(0, "h", "返回上层", "out a level", &[]),
    act(0, "l", "进入下层", "in a level", &[]),
    act(0, "Tab", "分组的会话 / 笔记", "a group's sessions / notes", &[]),
    act(0, "数字", "按编号选择", "select by number", &[]),
    act(0, "空格", "跳到会话 / 分组", "jump to a session / group", &[]),
    act(0, "a", "到全部并跳转", "all groups, jump", &[]),
    act(0, "`", "到左栏选分组", "pick a group on the rail", &[]),
    act(0, "g G", "顶部 / 底部", "top / bottom", &[]),
    act(0, "轻点", "选中,再点打开", "tap to select, again to open", &[]),
    act(1, "⏎", "打开", "open", &[]),
    act(1, "E", "附带便签打开", "open with the draft", S),
    act(1, "o", "打开 / 新建 router", "open / start the router", SGD),
    act(1, "i", "下一个待处理", "next to look at", &[]),
    act(1, "z", "全屏输出", "output", S),
    act(1, "x", "改动", "diff", S),
    act(1, "Z", "记录", "trace", S),
    act(1, "t", "终端", "terminal", S),
    act(1, "T", "新窗口", "new window", S),
    act(1, "y", "yazi", "yazi", S),
    act(2, "/", "搜索", "search", &[]),
    act(2, "w", "列出哪些", "which to list", &[]),
    act(2, "0", "所有会话", "every session", &[]),
    act(2, "F", "按状态筛选", "one state", &[]),
    act(2, "W", "进本组 / 回全部", "its group / all groups", SG),
    act(2, "S", "工作区", "workspaces", &[]),
    act(3, "n", "新建", "new", &[]),
    act(3, "N", "按当前配置新建", "new with current", &[]),
    act(3, "^N", "新分组", "new group", &[]),
    act(3, "e", "便签", "draft", S),
    act(3, "^G", "用编辑器写便签", "draft in the editor", S),
    act(3, "s", "回话", "reply", SGD),
    act(3, "p", "置顶", "pin", S),
    act(3, "u", "已读 / 未读", "seen · unseen", S),
    act(3, "v", "多选", "select", S),
    act(3, "V", "连续取消", "unselect a run", S),
    act(3, "r", "重命名", "rename", SG),
    act(3, "R", "重启", "restart", S),
    act(3, "D", "停止", "stop", S),
    act(3, "m", "移到分组", "move to a group", S),
    act(3, "Q", "移动到工作区", "send to a workspace", S),
    act(3, "⇧↑", "上移", "move up", SG),
    act(3, "⇧↓", "下移", "move down", SG),
    act(3, "f", "分叉", "fork", S),
    act(3, "H", "交接", "hand off", S),
    act(3, "X", "切换账号", "switch account", S),
    act(3, "A", "接管", "adopt", &[]),
    act(3, "M", "MCP", "MCP", S),
    act(3, "d", "删除", "delete", SG),
    act(3, "^Z", "撤销", "undo", &[]),
    act(3, "b", "回收站", "trash", &[]),
    act(3, "c", "复制", "copy", S),
    act(4, "$", "计费", "billing", &[]),
    act(4, "P", "插件", "plugins", &[]),
    act(4, "^S", "设置", "settings", &[]),
    act(4, "L", "布局", "layout", &[]),
    act(4, "^E", "特殊环境", "special environments", &[]),
    act(4, "^R", "刷新", "refresh", &[]),
    act(4, "^L", "重启 aca", "restart aca", &[]),
    act(4, "q q", "退出", "quit", &[]),
    act(5, "^Q", "返回 aca(或前缀 d)", "back to aca (or prefix d)", &[]),
    act(5, "前缀 a", "弹出 aca", "pop up aca", &[]),
    act(5, "^\\", "关闭弹出的 aca", "close the popped-up aca", &[]),
    act(5, "^`", "返回上一个会话", "previous session", &[]),
    act(6, "Tab", "补全", "complete", &[]),
    act(6, "→ /", "选择", "accept", &[]),
    act(6, "↓ ^F", "查找目录", "find a directory", &[]),
    act(6, "^W ^U", "删除一级 / 清空", "del level / clear", &[]),
    act(6, "^T", "shell / 切回", "shell / back", &[]),
    act(6, "+", "添加账号(n 菜单中)", "add account (n menu)", &[]),
];

/// Sections 5 and 6 are keys pressed somewhere else (in tmux, while making a new session): the sheet only lists them
pub fn elsewhere(a: &Act) -> bool {
    a.sec >= 5
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn keys_on_the_list_are_unique_and_names_stay_short() {
        let mut keys: Vec<&str> = ACTS.iter().filter(|a| !elsewhere(a)).map(|a| a.key).collect();
        let n = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), n);
        for a in ACTS {
            assert!(a.zh.width() <= 22 && a.en.width() <= 28, "{} 的名字太长", a.key);
        }
    }

    #[test]
    fn fit_says_where_each_works() {
        let f = ACTS.iter().find(|a| a.key == "f").unwrap();
        assert!(f.fits("session") && !f.fits("group") && !f.fits("global"));
        let d = ACTS.iter().find(|a| a.key == "d").unwrap();
        assert!(d.fits("session") && d.fits("group") && !d.fits("global"));
        assert!(ACTS.iter().find(|a| a.key == "N").unwrap().fits("global"));
        // o reaches the router of the row's group: a group's own on its row, a session's group's on its row, the desk on the
        // rail's row for all groups
        let o = ACTS.iter().find(|a| a.key == "o").unwrap();
        assert!(o.fits("group") && o.fits("desk") && o.fits("session") && !o.fits("global"));
        assert!(ACTS.iter().find(|a| a.key == "s").unwrap().fits("desk") && !d.fits("desk"));
    }
}
