//! ~/.config/aca/config.toml 里界面自己的那几段,都可以不写,比如
//!
//! ```toml
//! [new_group]
//! default_path = "~/workspace"
//!
//! [ime]
//! auto = false                 # 桌面上不自动切英文(默认切)
//!
//! [ui]
//! menu = ["work", "team"]      # n 菜单的顺序(见 tools.rs)
//! hide = ["official"]
//! ```
//!
//! 账号(`[accounts.*]`)和工具(`[tools.*]`)也在这个文件里,那两段归 aca-core 的 config.rs 读。
//! 用到时现读,改完不用重启。界面里 `^S` 的设置页改的也是这个文件(`set`,只动那一行)。

use std::path::PathBuf;

use toml::Value;

use aca_core::{t, tr};

/// `^N` 没配时的默认目录
const NEW_GROUP_DIR: &str = "~/workspace";

/// ~/.config/aca/config.toml(`ACA_HOME` 换地方,测试用)
pub fn path() -> PathBuf {
    aca_core::paths::config_dir().join("config.toml")
}

fn read() -> String {
    std::fs::read_to_string(path()).unwrap_or_default()
}

/// `^N` 起完名后 fzf 的第一行、Esc 回到输入框时填着的目录(`[new_group] default_path`)
pub fn new_group_dir() -> String {
    new_group_dir_in(&read())
}

/// 读不了 / 格式不对 / 没写都当没配
fn new_group_dir_in(text: &str) -> String {
    let t = text.parse::<toml::Table>().ok();
    t.as_ref()
        .and_then(|t| t.get("new_group")?.get("default_path")?.as_str())
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(NEW_GROUP_DIR)
        .to_string()
}

/// 桌面上 aca 自己切输入法(`[ime] auto`,不写 = 切;环境变量 ACA_IME 压过它,见 ime.rs)
pub fn ime_auto() -> bool {
    ime_auto_in(&read())
}

fn ime_auto_in(text: &str) -> bool {
    let t = text.parse::<toml::Table>().ok();
    t.as_ref().and_then(|t| t.get("ime")?.get("auto")?.as_bool()).unwrap_or(true)
}

/// `[section] key` 写的一串字(`[ui] menu = ["a", "b"]`);没写 / 写的不是这个样子 = 空
pub fn list(section: &str, key: &str) -> Vec<String> {
    list_in(&read(), section, key)
}

pub(crate) fn list_in(text: &str, section: &str, key: &str) -> Vec<String> {
    let t = text.parse::<toml::Table>().ok();
    let v = t.as_ref().and_then(|t| t.get(section)?.get(key)?.as_array());
    v.into_iter().flatten().filter_map(|x| x.as_str()).map(String::from).collect()
}

/// 这里管的几项写错了的(当没写),设置页列出来。整个文件解析不了由 theme 报,这里不重复
pub fn problems() -> Vec<String> {
    problems_in(&read())
}

fn problems_in(text: &str) -> Vec<String> {
    let Ok(t) = text.parse::<toml::Table>() else { return vec![] };
    let mut bad = vec![];
    if t.get("ime").and_then(|s| s.get("auto")).is_some_and(|v| !v.is_bool()) {
        bad.push(t!("config.toml [ime] auto:只认 true / false", "config.toml [ime] auto: only true / false").into());
    }
    if t.get("new_group").and_then(|s| s.get("default_path")).is_some_and(|v| !v.is_str()) {
        bad.push(
            t!("config.toml [new_group] default_path:要写成 \"目录\"", "config.toml [new_group] default_path: write it as \"<dir>\"")
                .into(),
        );
    }
    if let Some(e) = aca_core::config::login_problem_in(text) {
        bad.push(tr!("config.toml [login] 当成没写:{e}", "config.toml [login] is ignored: {e}"));
    }
    bad
}

/// 改一项:`[section] key = value`,别的内容、注释原样留着(见 `with_value`)。Err 是给底栏的一句话
pub fn set(section: &str, key: &str, value: Value) -> Result<(), String> {
    let path = path();
    let text = read();
    let new = with_value(&text, section, key, &value)
        .ok_or(t!("config.toml 格式不对,没记下(这次先用着)", "config.toml is malformed; not saved (in use for now)"))?;
    crate::state::write_atomic(&path, &new).map_err(|e| tr!("没记下:{e}", "not saved: {e}"))
}

/// 把 `[section]` 下的 key 改成 value:有这一行就换掉(缩进照旧),有这一段没这行就加在表头下面,都没有就在末尾加一段。
/// section 可以是嵌套的(`conductor.discord`)。
/// 原文件解析不了、或者改完读回来不对(写法太特别,比如用的是 `section.key = …` 这种点号写法),返回 None,不碰
pub fn with_value(text: &str, section: &str, key: &str, value: &Value) -> Option<String> {
    if !text.trim().is_empty() && text.parse::<toml::Table>().is_err() {
        return None;
    }
    let line = format!("{key} = {value}");
    let head = format!("[{section}]");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let bare = |l: &str| l.split('#').next().unwrap_or("").trim().to_string();
    match lines.iter().position(|l| bare(l) == head) {
        Some(h) => {
            let end = lines[h + 1..].iter().position(|l| bare(l).starts_with('[')).map_or(lines.len(), |i| h + 1 + i);
            let is_key = |l: &str| l.split_once('=').is_some_and(|(k, _)| k.trim() == key);
            let indent = |l: &str| l[..l.len() - l.trim_start().len()].to_string();
            match lines[h + 1..end].iter().position(|l| is_key(l)) {
                Some(i) => lines[h + 1 + i] = indent(&lines[h + 1 + i]) + &line,
                // 新加的那行跟着这一段里下一行缩进
                None => {
                    let pad = lines[h + 1..end].first().filter(|l| !bare(l).is_empty()).map(|l| indent(l)).unwrap_or_default();
                    lines.insert(h + 1, pad + &line);
                }
            }
        }
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(head);
            lines.push(line);
        }
    }
    let out = lines.join("\n") + "\n";
    let back = out.parse::<toml::Table>().ok()?;
    let mut at = &back;
    for part in section.split('.') {
        at = at.get(part)?.as_table()?;
    }
    (at.get(key)? == value).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_group_dir_defaults_to_workspace() {
        assert_eq!(new_group_dir_in(""), "~/workspace");
        assert_eq!(new_group_dir_in("[new_group]\ndefault_path = \" ~/code \"\n"), "~/code");
        assert_eq!(new_group_dir_in("[new_group]\ndefault_path = \"\""), "~/workspace");
        assert_eq!(new_group_dir_in("[new_group\nbroken"), "~/workspace");
        assert_eq!(new_group_dir_in("default_path = \"/top-level/ignored\""), "~/workspace");
    }

    #[test]
    fn ime_is_on_unless_turned_off() {
        assert!(ime_auto_in(""));
        assert!(!ime_auto_in("[ime]\nauto = false\n"));
        assert!(ime_auto_in("[ime]\nauto = \"no\"\n"));
        assert!(ime_auto_in("[ime\nbroken"));
        assert_eq!(problems_in("[ime]\nauto = \"no\"\n[new_group]\ndefault_path = 1\n").len(), 2);
        assert!(problems_in("[ime]\nauto = false\n[new_group]\ndefault_path = \"~/x\"\n").is_empty());
        // [login] 写错了:那一节当没写,设置页说一句
        let bad = problems_in("[login]\nremote = \"maybe\"\n");
        assert!(bad.len() == 1 && bad[0].starts_with("config.toml [login] 当成没写:") && bad[0].contains("maybe"), "{bad:?}");
        assert!(problems_in("[login]\nremote = \"yes\"\nbrowsers = [\"Firefox\"]\n").is_empty());
    }

    #[test]
    fn a_list_setting_is_read_as_strings() {
        let cfg = "[ui]\nmenu = [\"sub\", \"firm\", 3]\nhide = \"x\"\n";
        assert_eq!(list_in(cfg, "ui", "menu"), ["sub", "firm"]);
        assert!(list_in(cfg, "ui", "hide").is_empty());
        assert!(list_in("", "ui", "menu").is_empty());
    }

    #[test]
    fn setting_one_value_leaves_the_rest_alone() {
        let v = Value::Boolean(false);
        assert_eq!(with_value("", "ime", "auto", &v).unwrap(), "[ime]\nauto = false\n");
        let cfg = "# 我的设置\n[new_group]\ndefault_path = \"~/code\"  # 注释\n\n[accounts.claude]\nwork = \"~/.claude-work\"\n";
        assert_eq!(with_value(cfg, "ime", "auto", &v).unwrap(), format!("{cfg}\n[ime]\nauto = false\n"));
        let out = with_value(cfg, "new_group", "default_path", &Value::String("~/work".into())).unwrap();
        assert_eq!(out, cfg.replace("\"~/code\"  # 注释", "\"~/work\""));
        // 段在,键不在:加在表头下面
        let out = with_value("[ime] # 输入法\n\n[x]\nauto = 1\n", "ime", "auto", &v).unwrap();
        assert_eq!(out, "[ime] # 输入法\nauto = false\n\n[x]\nauto = 1\n");
        // 嵌套的段、缩进着写的:缩进照旧,别的段里同名的键不动
        let deck = "[conductor]\n  heartbeat_interval = 0\n  [conductor.discord]\n    listen_mode = \"all\"\n    user_id = 1\n[other]\nlisten_mode = \"x\"\n";
        let out = with_value(deck, "conductor.discord", "listen_mode", &Value::String("mentions".into())).unwrap();
        assert_eq!(out, deck.replace("    listen_mode = \"all\"", "    listen_mode = \"mentions\""));
        let out = with_value(deck, "conductor", "heartbeat_interval", &Value::Integer(15)).unwrap();
        assert_eq!(out, deck.replace("  heartbeat_interval = 0", "  heartbeat_interval = 15"));
        let out = with_value(deck, "conductor.discord", "ignore_replies_to_others", &v).unwrap();
        assert_eq!(out, deck.replace("  [conductor.discord]\n", "  [conductor.discord]\n    ignore_replies_to_others = false\n"));
        // 点号写法改不动(改完读回来重复定义),不碰
        assert!(with_value("ime.auto = true\n", "ime", "auto", &v).is_none());
        assert!(with_value("[ime\nbroken", "ime", "auto", &v).is_none());
    }
}
