//! The interface's own sections of ~/.config/aca/config.toml, all optional, e.g.
//!
//! ```toml
//! [new_group]
//! default_path = "~/workspace"
//!
//! [ime]
//! auto = false                 # don't auto-switch to English on desktop (on by default)
//!
//! [ui]
//! menu = ["work", "team"]      # order of the n menu (see tools.rs)
//! hide = ["official"]
//! ```
//!
//! Accounts (`[accounts.*]`) and tools (`[tools.*]`) are in this file too; those two sections are read by aca-core's config.rs.
//! Read fresh on each use, so edits need no restart. The `^S` settings page edits this same file (`set`, touching only that line).

use std::path::PathBuf;

use toml::Value;

use aca_core::{t, tr};

/// Default directory for `^N` when not configured
const NEW_GROUP_DIR: &str = "~/workspace";

/// ~/.config/aca/config.toml (`ACA_HOME` moves it, for tests)
pub fn path() -> PathBuf {
    aca_core::paths::config_dir().join("config.toml")
}

fn read() -> String {
    std::fs::read_to_string(path()).unwrap_or_default()
}

/// The directory on fzf's first line once `^N` has a name, and filled in when Esc goes back to the input box
/// (`[new_group] default_path`)
pub fn new_group_dir() -> String {
    new_group_dir_in(&read(), std::path::Path::new(&aca_core::paths::expand(NEW_GROUP_DIR)).is_dir())
}

/// Unreadable / malformed / unset all count as not configured. Then it's ~/workspace if that exists, otherwise (usually so on
/// a new machine) the home directory
fn new_group_dir_in(text: &str, workspace: bool) -> String {
    let t = text.parse::<toml::Table>().ok();
    t.as_ref()
        .and_then(|t| t.get("new_group")?.get("default_path")?.as_str())
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(if workspace { NEW_GROUP_DIR } else { "~" })
        .to_string()
}

/// On the desktop aca switches the input method itself (`[ime] auto`, unset = switch; the ACA_IME env var overrides it, see
/// ime.rs)
pub fn ime_auto() -> bool {
    ime_auto_in(&read())
}

fn ime_auto_in(text: &str) -> bool {
    let t = text.parse::<toml::Table>().ok();
    t.as_ref().and_then(|t| t.get("ime")?.get("auto")?.as_bool()).unwrap_or(true)
}

/// The list of strings at `[section] key` (`[ui] menu = ["a", "b"]`); unset / not of this shape = empty
pub fn list(section: &str, key: &str) -> Vec<String> {
    list_in(&read(), section, key)
}

pub(crate) fn list_in(text: &str, section: &str, key: &str) -> Vec<String> {
    let t = text.parse::<toml::Table>().ok();
    let v = t.as_ref().and_then(|t| t.get(section)?.get(key)?.as_array());
    v.into_iter().flatten().filter_map(|x| x.as_str()).map(String::from).collect()
}

/// Items managed here that are written wrong (treated as unset), for the settings page to list. A file that doesn't parse at
/// all is reported by theme, not repeated here
pub fn problems() -> Vec<String> {
    problems_in(&read())
}

fn problems_in(text: &str) -> Vec<String> {
    let Ok(t) = text.parse::<toml::Table>() else { return vec![] };
    let mut bad = vec![];
    if t.get("ime").and_then(|s| s.get("auto")).is_some_and(|v| !v.is_bool()) {
        bad.push(t!("config.toml [ime] auto:只能是 true 或 false", "config.toml [ime] auto: only true / false").into());
    }
    if t.get("new_group").and_then(|s| s.get("default_path")).is_some_and(|v| !v.is_str()) {
        bad.push(
            t!("config.toml [new_group] default_path:应写成 \"目录\"", "config.toml [new_group] default_path: write it as \"<dir>\"")
                .into(),
        );
    }
    if let Some(e) = aca_core::config::login_problem_in(text) {
        bad.push(tr!("config.toml [login] 已忽略:{e}", "config.toml [login] is ignored: {e}"));
    }
    if let Some(e) = aca_core::config::idle_problem_in(text) {
        bad.push(tr!("config.toml [idle] 已忽略,不会自动停止:{e}", "config.toml [idle] is ignored, nothing is auto-stopped: {e}"));
    }
    bad
}

/// Set one item: `[section] key = value`, leaving other content and comments as they are (see `with_value`). Err is one
/// sentence for the bottom bar (what wasn't saved mostly wasn't applied either: language, input method and launch args only
/// switch once saved, so no "using it for now")
pub fn set(section: &str, key: &str, value: Value) -> Result<(), String> {
    let path = path();
    let text = read();
    let new = with_value(&text, section, key, &value).ok_or(t!("config.toml 格式有误,未保存", "config.toml is malformed; not saved"))?;
    crate::state::write_atomic(&path, &new).map_err(|e| tr!("未保存:{e}", "not saved: {e}"))
}

/// Set key under `[section]` to value: replace the line if there is one (indent kept); if the section is there but the line
/// isn't, add it under the header; if neither, append a section at the end.
/// section may be nested (`conductor.discord`).
/// If the original doesn't parse, or reading back after the change doesn't match (an unusual style, e.g. dotted
/// `section.key = …`), return None and leave it alone
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
                // The new line takes the indent of the next line in this section
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
    fn new_group_dir_defaults_to_workspace_or_home_without_one() {
        assert_eq!(new_group_dir_in("", true), "~/workspace");
        assert_eq!(new_group_dir_in("", false), "~");
        assert_eq!(new_group_dir_in("[new_group]\ndefault_path = \" ~/code \"\n", false), "~/code");
        assert_eq!(new_group_dir_in("[new_group]\ndefault_path = \"\"", true), "~/workspace");
        assert_eq!(new_group_dir_in("[new_group\nbroken", true), "~/workspace");
        assert_eq!(new_group_dir_in("default_path = \"/top-level/ignored\"", false), "~");
    }

    #[test]
    fn ime_is_on_unless_turned_off() {
        assert!(ime_auto_in(""));
        assert!(!ime_auto_in("[ime]\nauto = false\n"));
        assert!(ime_auto_in("[ime]\nauto = \"no\"\n"));
        assert!(ime_auto_in("[ime\nbroken"));
        assert_eq!(problems_in("[ime]\nauto = \"no\"\n[new_group]\ndefault_path = 1\n").len(), 2);
        assert!(problems_in("[ime]\nauto = false\n[new_group]\ndefault_path = \"~/x\"\n").is_empty());
        // [login] written wrong: that section counts as unset, the settings page says so
        let bad = problems_in("[login]\nremote = \"maybe\"\n");
        assert!(bad.len() == 1 && bad[0].starts_with("config.toml [login] 已忽略:") && bad[0].contains("maybe"), "{bad:?}");
        assert!(problems_in("[login]\nremote = \"yes\"\nbrowsers = [\"Firefox\"]\n").is_empty());
        // [idle] written wrong: nothing auto-stops, the settings page says so
        let bad = problems_in("[idle]\nstop_after = \"三小时\"\n");
        assert!(bad.len() == 1 && bad[0].starts_with("config.toml [idle] 已忽略,不会自动停止:") && bad[0].contains("三小时"), "{bad:?}");
        assert!(problems_in("[idle]\nstop_after = \"3h\"\n").is_empty());
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
        // Section present, key missing: added under the header
        let out = with_value("[ime] # 输入法\n\n[x]\nauto = 1\n", "ime", "auto", &v).unwrap();
        assert_eq!(out, "[ime] # 输入法\nauto = false\n\n[x]\nauto = 1\n");
        // Nested sections, written indented: indent kept, same-named keys in other sections untouched
        let deck = "[conductor]\n  heartbeat_interval = 0\n  [conductor.discord]\n    listen_mode = \"all\"\n    user_id = 1\n[other]\nlisten_mode = \"x\"\n";
        let out = with_value(deck, "conductor.discord", "listen_mode", &Value::String("mentions".into())).unwrap();
        assert_eq!(out, deck.replace("    listen_mode = \"all\"", "    listen_mode = \"mentions\""));
        let out = with_value(deck, "conductor", "heartbeat_interval", &Value::Integer(15)).unwrap();
        assert_eq!(out, deck.replace("  heartbeat_interval = 0", "  heartbeat_interval = 15"));
        let out = with_value(deck, "conductor.discord", "ignore_replies_to_others", &v).unwrap();
        assert_eq!(out, deck.replace("  [conductor.discord]\n", "  [conductor.discord]\n    ignore_replies_to_others = false\n"));
        // Dotted style can't be edited (reading back gives a duplicate definition), left alone
        assert!(with_value("ime.auto = true\n", "ime", "auto", &v).is_none());
        assert!(with_value("[ime\nbroken", "ime", "auto", &v).is_none());
    }
}
