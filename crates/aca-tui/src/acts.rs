//! 主列表的动作一张表:键、叫什么、在哪种行上有效。`:` 面板从这里列 aca 自己的动作(挑中一行 = 在主列表上按了那个键),
//! `?` 速查的行是手排的(几个键并一行),测试保证两边的键对得上。名字只写两到四个字:面板一行一个,长了挤掉右边的来源。

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use aca_core::i18n::{Lang, lang};

/// 一个动作:键(写法同 `?`:单个字、`^N`、`Tab`)、中英文名、在哪种行上有效(空 = 都行;"session" 登记的会话行、"group" 分组行)
pub struct Act {
    pub key: &'static str,
    zh: &'static str,
    en: &'static str,
    pub on: &'static [&'static str],
}

impl Act {
    pub fn name(&self) -> &'static str {
        if lang() == Lang::En { self.en } else { self.zh }
    }

    /// 高亮的行是 on(session / group / global)时能用
    pub fn fits(&self, on: &str) -> bool {
        self.on.is_empty() || self.on.contains(&on)
    }

    /// 它的键按出来是什么(主列表收到的那个事件)
    pub fn event(&self) -> Option<KeyEvent> {
        let mut cs = self.key.chars();
        match (cs.next(), cs.next(), cs.next()) {
            _ if self.key == "Tab" => Some(KeyEvent::from(KeyCode::Tab)),
            (Some('^'), Some(c), None) => Some(KeyEvent::new(KeyCode::Char(c.to_ascii_lowercase()), KeyModifiers::CONTROL)),
            (Some(c), None, _) => Some(KeyEvent::from(KeyCode::Char(c))),
            _ => None,
        }
    }

    /// 按下的是它的键
    pub fn hit(&self, k: KeyEvent) -> bool {
        self.event().is_some_and(|e| e.code == k.code && e.modifiers.contains(KeyModifiers::CONTROL) == k.modifiers.contains(KeyModifiers::CONTROL))
    }
}

const fn act(key: &'static str, zh: &'static str, en: &'static str, on: &'static [&'static str]) -> Act {
    Act { key, zh, en, on }
}

const S: &[&str] = &["session"];
const SG: &[&str] = &["session", "group"];

/// 面板里的顺序:先是对这一行做的事,再是打开、筛选、整个界面的
pub const ACTS: &[Act] = &[
    act("n", "新建", "new", &[]),
    act("N", "按当前配置新建", "new with current", &[]),
    act("^N", "新分组", "new group", &[]),
    act("e", "便签", "note", S),
    act("^G", "用编辑器写便签", "note in the editor", S),
    act("s", "发送", "send", S),
    act("p", "置顶", "pin", S),
    act("u", "已读 / 未读", "seen · unseen", S),
    act("v", "多选", "select", S),
    act("V", "连续取消", "unselect a run", S),
    act("r", "重命名", "rename", S),
    act("R", "重启", "restart", S),
    act("D", "停止", "stop", S),
    act("m", "移到分组", "move to a group", S),
    act("f", "分叉", "fork", S),
    act("H", "交接", "hand off", S),
    act("X", "切换账号", "switch account", S),
    act("A", "接管", "adopt", &[]),
    act("M", "MCP", "MCP", S),
    act("P", "插件", "plugins", &[]),
    act("d", "删除", "delete", SG),
    act("^Z", "撤销", "undo", &[]),
    act("b", "回收站", "trash", &[]),
    act("c", "复制回复", "copy the reply", S),
    act("B", "信息", "info", S),
    act("C", "选取文本", "pick text", S),
    act("E", "附带便签打开", "open with a note", S),
    act("i", "下一个待处理", "next to look at", &[]),
    act("z", "全屏输出", "output", S),
    act("x", "改动", "diff", S),
    act("Z", "记录", "trace", S),
    act("t", "终端", "terminal", S),
    act("T", "新窗口", "new window", S),
    act("y", "yazi", "yazi", S),
    act("/", "搜索", "search", &[]),
    act("w", "未停止", "open ones", &[]),
    act("a", "全部", "all", &[]),
    act("*", "切换天数", "days", &[]),
    act("F", "按状态筛选", "one state", &[]),
    act("Tab", "分组列表", "group list", &[]),
    act("W", "仅本组", "this group only", SG),
    act("o", "折叠", "fold", SG),
    act("O", "全部折叠", "fold all", &[]),
    act("L", "布局", "layout", &[]),
    act("$", "计费", "billing", &[]),
    act("^S", "设置", "settings", &[]),
    act("^R", "刷新", "refresh", &[]),
    act("^L", "重启 aca", "restart aca", &[]),
    act("S", "工作区", "workspaces", &[]),
    act("Q", "移动到工作区", "send to a workspace", S),
    act("?", "快捷键", "keys", &[]),
];

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn every_key_turns_into_the_event_the_list_gets_and_names_stay_short() {
        for a in ACTS {
            let e = a.event().unwrap_or_else(|| panic!("{} 没法按", a.key));
            assert!(a.hit(e), "{}", a.key);
            assert!(a.zh.width() <= 14 && a.en.width() <= 20, "{} 的名字太长", a.key);
        }
        assert_eq!(ACTS.iter().find(|a| a.key == "^N").unwrap().event(), Some(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)));
        assert!(ACTS.iter().find(|a| a.key == "F").unwrap().hit(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT)));
        assert!(!ACTS.iter().find(|a| a.key == "n").unwrap().hit(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn keys_are_unique_and_fit_says_where_each_works() {
        let mut keys: Vec<&str> = ACTS.iter().map(|a| a.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), ACTS.len());
        let f = ACTS.iter().find(|a| a.key == "f").unwrap();
        assert!(f.fits("session") && !f.fits("group") && !f.fits("global"));
        let d = ACTS.iter().find(|a| a.key == "d").unwrap();
        assert!(d.fits("session") && d.fits("group") && !d.fits("global"));
        assert!(ACTS.iter().find(|a| a.key == "N").unwrap().fits("global"));
    }
}
