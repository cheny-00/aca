//! Your decisions about plugins. A plugin says in its manifest what it would like; you decide in plugins.toml (as in
//! kitty: a kitten doesn't bind its own keys, and you list which commands each remote-control password may use):
//!
//! - **Which tools agents get**: `tools = ["list"]`. Not written = the commands whose `expose` says `agent` in the manifest;
//!   `agent?` ones (that change things on your machine) are given only when you name them. aca checks this both when it
//!   lists tools and when one is actually called. This is the list of tools for agents, not a sandbox: an agent with a
//!   shell can still type `aca` commands.
//! - **Keys in the `:` menu**: `keys = { tasks = "T", run = "" }` (command / pane / view id → one character, empty = no key).
//!   The rest follow the manifest's `[[keys]]`. When two plugins want the same key: yours wins; if both come from manifests,
//!   the one registered first gets it. The one left without is named (plugins page, `aca plugin ls`), instead of quietly
//!   losing it.
//! - **Hotkeys inside sessions**: `hotkeys = { pick = "M-c", pad = "" }` (pane id → a tmux key name, empty = none).
//!   The rest follow the manifest's `[[hotkeys]]`; you can give one to a pane the manifest gave none. Settled the same way
//!   as above.

use serde_json::json;

use super::decl::{self, Want};
use super::manifest::hotkey_ok;
use super::{Entry, MENU_KEYS_TAKEN, registry};
use crate::paths::Res;
use crate::tr;

/// The tools this plugin gives agents now (command ids, in manifest order)
pub fn agent_tools(e: &Entry, w: Option<&Want>) -> Vec<String> {
    match w.and_then(|w| w.tools.as_ref()) {
        None => e.tools.clone(),
        Some(l) => e.tools.iter().chain(&e.tools_ask).filter(|t| l.contains(t)).cloned().collect(),
    }
}

/// Everything that can go to agents: (command id, given now, needs you to name it)
pub fn tool_choices(id: &str) -> Res<Vec<(String, bool, bool)>> {
    let e = find(id)?;
    let now = agent_tools(&e, Some(&decl::want(id)));
    let all = e.tools.iter().map(|t| (t, false)).chain(e.tools_ask.iter().map(|t| (t, true)));
    Ok(all.map(|(t, ask)| (t.clone(), now.contains(t), ask)).collect())
}

fn find(id: &str) -> Res<Entry> {
    registry().into_iter().find(|e| e.id == id).ok_or_else(|| tr!("没有插件 {id}", "no plugin {id}"))
}

/// Give / don't give agents a command as a tool (written to `tools` in plugins.toml; not written when it's the same as
/// the manifest's default). Returns what's given now
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

/// Back to the manifest's defaults
pub fn reset_tools(id: &str) -> Res<Vec<String>> {
    let e = find(id)?;
    decl::set(id, "tools", None)?;
    tools_changed();
    Ok(e.tools)
}

/// The tool list changed: the commands server under the gateway restarts the next time someone needs it, and connected agents
/// fetch the list again (Claude switches at once, Codex only after a restart)
fn tools_changed() {
    let _ = crate::gateway::ctl(json!({ "cmd": "restart", "server": "tools" }));
    let _ = crate::gateway::ctl(json!({ "cmd": "changed" }));
}

/// What key a menu entry (or a pane's hotkey) wants: the manifest's, and yours (None = not written; Some("") = no key)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyWant<K = char> {
    pub plugin: String,
    pub id: String,
    pub title: String,
    pub manifest: Option<K>,
    pub mine: Option<String>,
}

/// Hands out keys: yours first, then the manifests' (each in registration order). Returns the key each entry got, and
/// the ones that got none / were written wrong (plugin id, one sentence)
pub fn settle(wants: &[KeyWant]) -> (Vec<Option<char>>, Vec<(String, String)>) {
    settle_by(wants, key_ok, "keys")
}

/// Hands out hotkeys, the same way
pub fn settle_hotkeys(wants: &[KeyWant<String>]) -> (Vec<Option<String>>, Vec<(String, String)>) {
    settle_by(wants, |k| hotkey_ok(k).map(|_| k.to_string()), "hotkeys")
}

/// `ok`: whether the key you wrote works; `field`: which field of plugins.toml this kind of key goes in (the note tells
/// you where to change it)
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

/// A key usable in the `:` menu
fn key_ok(k: &str) -> Res<char> {
    let mut cs = k.chars();
    match (cs.next(), cs.next()) {
        (Some(c), None) if !c.is_whitespace() && !MENU_KEYS_TAKEN.contains(&k) => Ok(c),
        (Some(_), None) if !k.trim().is_empty() => Err(tr!("{k} 已被 : 菜单占用", "{k} is taken by the : menu")),
        _ => Err(tr!("键应为单个字符:「{k}」", "a key is one character: “{k}”")),
    }
}

/// Changes one entry's key (written to `keys` in plugins.toml): Some("M") switches to M, Some("") no key, None back to
/// the manifest's
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

/// Changes a pane's hotkey (written to `hotkeys` in plugins.toml): Some("M-n") switches to it, Some("") none, None back
/// to the manifest's. A pane the manifest gave no hotkey can get one too. tmux follows the change
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
            // Two plugins' manifests want the same key: the one registered first gets it, the other is named and told to change it
            // in hotkeys
            let (got, notes) = settle_hotkeys(&[h("a", "pick", Some("C-]"), None), h("b", "pad", Some("C-]"), None)]);
            assert_eq!(got, [Some("C-]".to_string()), None]);
            assert!(
                notes[0].1.contains("键 C-] 已分配给 a「pick」,b「pad」未分配到") && notes[0].1.contains("hotkeys = { pad = "),
                "{notes:?}"
            );
            // Yours first; none; you give one the manifest didn't; a wrongly written one is named and gets no key
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
            // Both plugins' manifests want m: the one registered first gets it, the other is named
            let (got, notes) = settle(&[w("a", "mounts", Some('m'), None), w("b", "mail", Some('m'), None)]);
            assert_eq!(got, [Some('m'), None]);
            assert_eq!(notes.len(), 1);
            assert_eq!(notes[0].0, "b");
            assert!(
                notes[0].1.contains("键 m 已分配给 a「mounts」,b「mail」未分配到") && notes[0].1.contains("keys = { mail = "),
                "{notes:?}"
            );
            // You wrote m for b: yours first, a's manifest default gets nothing
            let (got, notes) = settle(&[w("a", "mounts", Some('m'), None), w("b", "mail", Some('m'), Some("m"))]);
            assert_eq!((got, notes[0].0.as_str()), (vec![None, Some('m')], "a"));
            // Changed, no key, written wrong
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
