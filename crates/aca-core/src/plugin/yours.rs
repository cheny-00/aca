//! 你对插件的决定。插件在清单里说它想要什么,你在 plugins.toml 里说了算(像 kitty:kitten 不自己绑键,
//! 远程控制每个口令能用哪些命令由你列):
//!
//! - **给 agent 哪些工具**:`tools = ["list"]`。没写 = 清单里 `expose` 写 `agent` 的那些;写 `agent?` 的
//!   (会动你机器上东西的)要你点了名才给。aca 列工具、真去调的时候都照这个查。
//!   这是给 agent 的工具清单,不是沙箱:agent 有 shell 的话照样能敲 `aca` 命令。
//! - **`:` 菜单里的键**:`keys = { tasks = "T", run = "" }`(能力 / 窗格 / 视图的 id → 一个字符,空的 = 不要键)。
//!   没写的照清单的 `[[keys]]`。两个插件要同一个键:你写的先拿;都是清单写的,先登记的那个拿。
//!   没拿到的点名出来(插件页、`aca plugin ls`),不再悄悄没了。
//! - **会话里面的热键**:`hotkeys = { pick = "M-c", pad = "" }`(窗格的 id → tmux 的键名,空的 = 不要)。
//!   没写的照清单的 `[[hotkeys]]`;清单没给热键的窗格你也能给一个。分法和上面一样。

use serde_json::json;

use super::decl::{self, Want};
use super::manifest::hotkey_ok;
use super::{Entry, MENU_KEYS_TAKEN, registry};
use crate::paths::Res;
use crate::tr;

/// 这个插件现在给 agent 的工具(能力 id,清单里的顺序)
pub fn agent_tools(e: &Entry, w: Option<&Want>) -> Vec<String> {
    match w.and_then(|w| w.tools.as_ref()) {
        None => e.tools.clone(),
        Some(l) => e.tools.iter().chain(&e.tools_ask).filter(|t| l.contains(t)).cloned().collect(),
    }
}

/// 能给 agent 的全部:(能力 id, 现在给没给, 要不要你点名)
pub fn tool_choices(id: &str) -> Res<Vec<(String, bool, bool)>> {
    let e = find(id)?;
    let now = agent_tools(&e, Some(&decl::want(id)));
    let all = e.tools.iter().map(|t| (t, false)).chain(e.tools_ask.iter().map(|t| (t, true)));
    Ok(all.map(|(t, ask)| (t.clone(), now.contains(t), ask)).collect())
}

fn find(id: &str) -> Res<Entry> {
    registry().into_iter().find(|e| e.id == id).ok_or_else(|| tr!("没有插件 {id}", "no plugin {id}"))
}

/// 给 / 不给 agent 一个能力当工具(写进 plugins.toml 的 `tools`;跟清单默认的一样就不写)。返回现在给的
pub fn allow(id: &str, tool: &str, on: bool) -> Res<Vec<String>> {
    let e = find(id)?;
    if !e.tools.iter().chain(&e.tools_ask).any(|t| t == tool) {
        return Err(tr!(
            "{id} 的 {tool} 不能提供给 agent(清单的 expose 中没有 agent / agent?)",
            "{id}'s {tool} can't go to agents (its expose has neither agent nor agent?)"
        ));
    }
    let mut now = agent_tools(&e, Some(&decl::want(id)));
    now.retain(|t| t != tool);
    if on {
        now.push(tool.to_string());
    }
    let order: Vec<&String> = e.tools.iter().chain(&e.tools_ask).collect();
    now.sort_by_key(|t| order.iter().position(|o| *o == t));
    let value = (now != e.tools).then(|| toml::Value::Array(now.iter().map(|t| t.clone().into()).collect()));
    decl::set(id, "tools", value)?;
    tools_changed();
    Ok(now)
}

/// 回到清单默认的那些
pub fn reset_tools(id: &str) -> Res<Vec<String>> {
    let e = find(id)?;
    decl::set(id, "tools", None)?;
    tools_changed();
    Ok(e.tools)
}

/// 工具清单变了:gateway 下面那个能力服务下次有人要时重起,连着的 agent 重新拉列表(Claude 当场换,Codex 重启才换)
fn tools_changed() {
    let _ = crate::gateway::ctl(json!({ "cmd": "restart", "server": "tools" }));
    let _ = crate::gateway::ctl(json!({ "cmd": "changed" }));
}

/// 菜单里的一项(或者一个窗格的热键)要什么键:清单写的,和你写的(None = 没写;Some("") = 不要键)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyWant<K = char> {
    pub plugin: String,
    pub id: String,
    pub title: String,
    pub manifest: Option<K>,
    pub mine: Option<String>,
}

/// 分键:你写的先拿,再是清单写的(都按登记的顺序)。返回每一项拿到的键,和没拿到 / 写得不对的(插件 id, 一句话)
pub fn settle(wants: &[KeyWant]) -> (Vec<Option<char>>, Vec<(String, String)>) {
    settle_by(wants, key_ok, "keys")
}

/// 分热键,一样的分法
pub fn settle_hotkeys(wants: &[KeyWant<String>]) -> (Vec<Option<String>>, Vec<(String, String)>) {
    settle_by(wants, |k| hotkey_ok(k).map(|_| k.to_string()), "hotkeys")
}

/// `ok`:你写的那个键行不行;`field`:plugins.toml 里这种键写在哪个字段(提醒里告诉你去哪改)
fn settle_by<K: Clone + Eq + std::hash::Hash + std::fmt::Display>(
    wants: &[KeyWant<K>],
    ok: impl Fn(&str) -> Res<K>,
    field: &str,
) -> (Vec<Option<K>>, Vec<(String, String)>) {
    let mut got: Vec<Option<K>> = vec![None; wants.len()];
    let mut notes = vec![];
    let mut owner: std::collections::HashMap<K, usize> = std::collections::HashMap::new();
    let mut take = |i: usize, c: K, got: &mut Vec<Option<K>>, notes: &mut Vec<(String, String)>| match owner.get(&c) {
        Some(&j) => {
            let (w, o) = (&wants[i], &wants[j]);
            notes.push((
                w.plugin.clone(),
                tr!(
                    "键 {c} 已分配给 {}「{}」,{}「{}」未分配到(在 plugins.toml 中为它换一个:[plugins.\"{}\"] {field} = {{ {} = \"…\" }})",
                    "key {c} went to {} “{}”; {} “{}” didn't get it (give it another in plugins.toml: [plugins.\"{}\"] {field} = {{ {} = \"…\" }})",
                    o.plugin,
                    o.title,
                    w.plugin,
                    w.title,
                    w.plugin,
                    w.id
                ),
            ));
        }
        None => {
            owner.insert(c.clone(), i);
            got[i] = Some(c);
        }
    };
    for (i, w) in wants.iter().enumerate() {
        let Some(k) = &w.mine else { continue };
        if k.is_empty() {
            continue;
        }
        match ok(k) {
            Ok(c) => take(i, c, &mut got, &mut notes),
            Err(e) => notes.push((w.plugin.clone(), tr!("{}「{}」:{e}", "{} “{}”: {e}", w.plugin, w.title))),
        }
    }
    for (i, w) in wants.iter().enumerate() {
        if let (None, Some(c)) = (&w.mine, &w.manifest) {
            take(i, c.clone(), &mut got, &mut notes);
        }
    }
    (got, notes)
}

/// 一个能在 `:` 菜单里用的键
fn key_ok(k: &str) -> Res<char> {
    let mut cs = k.chars();
    match (cs.next(), cs.next()) {
        (Some(c), None) if !c.is_whitespace() && !MENU_KEYS_TAKEN.contains(&k) => Ok(c),
        (Some(_), None) if !k.trim().is_empty() => Err(tr!("{k} 已被 : 菜单占用", "{k} is taken by the : menu")),
        _ => Err(tr!("键应为单个字符:「{k}」", "a key is one character: “{k}”")),
    }
}

/// 改一个的键(写进 plugins.toml 的 `keys`):Some("M") 换成 M,Some("") 不要键,None 回到清单的
pub fn set_key(id: &str, target: &str, key: Option<&str>) -> Res<()> {
    let e = find(id)?;
    let (m, _) = super::read_manifest(std::path::Path::new(&e.path))?;
    let in_menu = m.actions.iter().any(|a| a.id == target && a.exposed("menu"))
        || m.panes.iter().any(|p| p.id == target)
        || m.views.iter().any(|v| v.id == target);
    if !in_menu {
        return Err(tr!(
            "{id} 没有 {target}(只有 : 菜单中的能力、窗格、视图能设键)",
            "{id} has no {target} (only : menu commands, panes and views take keys)"
        ));
    }
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        key_ok(k)?;
    }
    let mut keys = decl::want(id).keys;
    match key {
        Some(k) => keys.insert(target.to_string(), k.to_string()),
        None => keys.remove(target),
    };
    let value = (!keys.is_empty()).then(|| toml::Value::Table(keys.into_iter().map(|(k, v)| (k, v.into())).collect()));
    decl::set(id, "keys", value)
}

/// 改一个窗格的热键(写进 plugins.toml 的 `hotkeys`):Some("M-n") 换成它,Some("") 不要,None 回到清单的。
/// 清单没给热键的窗格也能给。改完 tmux 上跟着换
pub fn set_hotkey(id: &str, pane: &str, key: Option<&str>) -> Res<()> {
    let e = find(id)?;
    let (m, _) = super::read_manifest(std::path::Path::new(&e.path))?;
    if !m.panes.iter().any(|p| p.id == pane) {
        return Err(tr!("{id} 没有窗格 {pane}(热键只能打开窗格)", "{id} has no pane {pane} (hotkeys open panes only)"));
    }
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        hotkey_ok(k)?;
    }
    let mut keys = decl::want(id).hotkeys;
    match key {
        Some(k) => keys.insert(pane.to_string(), k.to_string()),
        None => keys.remove(pane),
    };
    let value = (!keys.is_empty()).then(|| toml::Value::Table(keys.into_iter().map(|(k, v)| (k, v.into())).collect()));
    decl::set(id, "hotkeys", value)?;
    super::bind_hotkeys();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkeys_are_dealt_like_menu_keys_and_the_note_says_where_to_change_one() {
        crate::i18n::with(crate::i18n::Lang::Zh, || {
            let h = |plugin: &str, id: &str, manifest: Option<&str>, mine: Option<&str>| KeyWant {
                plugin: plugin.into(),
                id: id.into(),
                title: id.into(),
                manifest: manifest.map(String::from),
                mine: mine.map(String::from),
            };
            // 两个插件的清单要同一个键:先登记的拿,后面那个点名,告诉它去 hotkeys 里换
            let (got, notes) = settle_hotkeys(&[h("a", "pick", Some("C-]"), None), h("b", "pad", Some("C-]"), None)]);
            assert_eq!(got, [Some("C-]".to_string()), None]);
            assert!(
                notes[0].1.contains("键 C-] 已分配给 a「pick」,b「pad」未分配到") && notes[0].1.contains("hotkeys = { pad = "),
                "{notes:?}"
            );
            // 你写的先拿;不要;清单没给的你给一个;写得不对的点名,键不给
            let (got, notes) = settle_hotkeys(&[
                h("a", "pick", Some("C-]"), Some("")),
                h("b", "pad", None, Some("C-]")),
                h("c", "x", Some("M-y"), Some("y")),
            ]);
            assert_eq!(got, [None, Some("C-]".to_string()), None]);
            assert!(notes.len() == 1 && notes[0].1.contains("tmux"), "{notes:?}");
        });
    }

    fn w(plugin: &str, id: &str, manifest: Option<char>, mine: Option<&str>) -> KeyWant {
        KeyWant { plugin: plugin.into(), id: id.into(), title: id.into(), manifest, mine: mine.map(String::from) }
    }

    #[test]
    fn your_keys_win_then_the_manifests_in_order_and_whoever_loses_is_named() {
        crate::i18n::with(crate::i18n::Lang::Zh, || {
            // 两个插件的清单都要 m:先登记的拿到,后面那个点名
            let (got, notes) = settle(&[w("a", "mounts", Some('m'), None), w("b", "mail", Some('m'), None)]);
            assert_eq!(got, [Some('m'), None]);
            assert_eq!(notes.len(), 1);
            assert_eq!(notes[0].0, "b");
            assert!(
                notes[0].1.contains("键 m 已分配给 a「mounts」,b「mail」未分配到") && notes[0].1.contains("keys = { mail = "),
                "{notes:?}"
            );
            // 你给 b 写了 m:你写的先拿,a 的清单默认没拿到
            let (got, notes) = settle(&[w("a", "mounts", Some('m'), None), w("b", "mail", Some('m'), Some("m"))]);
            assert_eq!((got, notes[0].0.as_str()), (vec![None, Some('m')], "a"));
            // 换一个、不要键、写错了
            let (got, notes) = settle(&[
                w("a", "mounts", Some('m'), Some("M")),
                w("a", "tasks", Some('t'), Some("")),
                w("a", "run", Some('r'), Some("rr")),
                w("a", "find", None, Some("/")),
            ]);
            assert_eq!(got, [Some('M'), None, None, None]);
            assert_eq!(notes.len(), 2, "{notes:?}");
            assert!(notes[0].1.contains("单个字符") && notes[1].1.contains(": 菜单占用"), "{notes:?}");
        });
    }
}
