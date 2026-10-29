//! `~/.config/aca/plugins.toml`: which plugins you want, the way vim-plug lists them in your vimrc. Keep it with your
//! dotfiles; on another machine it's a single `aca plugin sync`.
//!
//! ```toml
//! [plugins."aca.delegate"]
//! source = "~/src/aca/plugins/delegate"   # a local directory = link; a git URL / owner/repo = clone
//! agents = ["claude:*", "codex:work"]    # which "tool[:account]" get what it brings; not written = every agent the manifest names
//! settings = { max_continue = 3 }        # overrides the manifest's defaults
//! tools = ["dispatch"]                   # which commands agents get as tools; not written = those marked agent in the manifest
//!                                        # (agent? ones must be named)
//! keys = { tasks = "T", run = "" }       # keys in the : menu: command / pane / view → key, "" = no key; the rest follow the manifest
//! hotkeys = { pad = "M-n" }              # hotkeys inside sessions: pane → tmux key name, "" = none; the rest follow the manifest
//!
//! [plugins."owner/gh-glance"]
//! source = "owner/gh-glance"
//! ref = "v1.2"                           # pinned to a tag / branch / commit
//! enabled = false                        # disabled for now
//! ```
//!
//! `plugins.lock` records the commit of each plugin installed from git: `sync` installs exactly those (another machine
//! ends up the same), and `update` changes it after pulling.
//!
//! Installing, removing, enabling and disabling from the command line or the interface update this file as they go (only
//! that plugin's lines; everything else, comments included, stays as it is); when you edit it by hand, `aca plugin sync`
//! makes this machine match it. If the file doesn't exist yet, one is written from the registered plugins the first time
//! it's needed.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths::{self, Res};
use crate::tr;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Want {
    pub source: String,
    #[serde(rename = "ref")]
    pub rev: String,
    pub enabled: Option<bool>,
    /// Which "tool[:account]" get what it brings: `claude`, `claude:*` (any account of that tool), `codex:work`; not written = all
    pub agents: Vec<String>,
    pub settings: toml::Table,
    /// Which commands agents get as tools (command ids); None = as the manifest says (`agent` ones yes, `agent?` ones no)
    pub tools: Option<Vec<String>>,
    /// Keys in the `:` menu: command / pane / view id → one character; empty string = no key. The rest follow the manifest's
    /// `[[keys]]`
    pub keys: BTreeMap<String, String>,
    /// Hotkeys inside sessions: pane id → tmux key name; empty string = none. The rest follow the manifest's `[[hotkeys]]`
    pub hotkeys: BTreeMap<String, String>,
}

impl Want {
    pub fn on(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// Whether this session gets it: `tool` is the session's tool name (the one after `aca new`), `kind` is which built-in
    /// tool it starts as (claude / codex); either matches. `account` is the account name
    pub fn reaches(&self, tool: &str, kind: &str, account: &str) -> bool {
        let named = |t: &str| t == tool || t == kind;
        self.agents.is_empty()
            || self.agents.iter().any(|a| match a.split_once(':') {
                Some((t, "*")) => named(t),
                Some((t, acc)) => named(t) && acc == account,
                None => named(a),
            })
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct File {
    plugins: BTreeMap<String, Want>,
}

pub fn decl_file() -> PathBuf {
    paths::config_dir().join("plugins.toml")
}

pub fn lock_file() -> PathBuf {
    paths::config_dir().join("plugins.lock")
}

/// Reads plugins.toml. No such file = empty; broken = Err (saying what's wrong)
pub fn wants() -> Res<BTreeMap<String, Want>> {
    match std::fs::read_to_string(decl_file()) {
        Ok(text) => parse_decl(&text),
        Err(_) => Ok(BTreeMap::new()),
    }
}

fn parse_decl(text: &str) -> Res<BTreeMap<String, Want>> {
    toml::from_str::<File>(text)
        .map(|f| f.plugins)
        .map_err(|e| tr!("无法解析 plugins.toml:{}", "can't parse plugins.toml: {}", e.message()))
}

/// One plugin's section in plugins.toml; not written / broken file = the default (enabled, given to all, no settings)
pub fn want(id: &str) -> Want {
    wants().ok().and_then(|mut w| w.remove(id)).unwrap_or_default()
}

/// Settings handed to plugin commands: the manifest's defaults, with what plugins.toml says on top
pub fn settings(id: &str, m: &super::Manifest) -> toml::Table {
    let mut all = m.defaults();
    all.extend(want(id).settings);
    all
}

/// The header of the `[plugins."<id>"]` section (an id without dots, only letters, digits, _ -, may also be written
/// unquoted)
fn is_head(line: &str, id: &str) -> bool {
    let bare = line.split('#').next().unwrap_or("").trim();
    bare == format!("[plugins.\"{id}\"]")
        || (id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') && bare == format!("[plugins.{id}]"))
}

fn is_any_head(line: &str) -> bool {
    line.trim_start().starts_with('[')
}

/// Where the section whose header is at `h` ends (exclusive): before the next header; the comments and blank lines right
/// above it belong to the next section
fn block_end(lines: &[String], h: usize) -> usize {
    let mut end = lines[h + 1..].iter().position(|l| is_any_head(l)).map_or(lines.len(), |i| h + 1 + i);
    if end < lines.len() {
        while end > h + 1 && (lines[end - 1].trim().is_empty() || lines[end - 1].trim_start().starts_with('#')) {
            end -= 1;
        }
    }
    end
}

/// Sets one key of a plugin to value (None = remove it), leaving other lines and comments as they are; adds the section
/// at the end if there isn't one. If reading it back doesn't match (the value got written over several lines or the
/// like), the whole section is rewritten from the current values
pub(crate) fn edit(text: &str, id: &str, key: &str, value: Option<&toml::Value>) -> Res<String> {
    let current = parse_decl(text)?;
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let is_key = |l: &str| l.split('#').next().unwrap_or("").split_once('=').is_some_and(|(k, _)| k.trim().trim_matches('"') == key);
    match lines.iter().position(|l| is_head(l, id)) {
        Some(h) => {
            let end = block_end(&lines, h);
            let at = lines[h + 1..end].iter().position(|l| is_key(l)).map(|i| h + 1 + i);
            match (at, value) {
                (Some(i), Some(v)) => lines[i] = format!("{key} = {v}"),
                (Some(i), None) => {
                    lines.remove(i);
                }
                (None, Some(v)) => {
                    // Goes after the section's last non-empty line
                    let last = (h..end).rev().find(|&i| !lines[i].trim().is_empty()).unwrap_or(h);
                    lines.insert(last + 1, format!("{key} = {v}"));
                }
                (None, None) => {}
            }
        }
        None => {
            let Some(v) = value else { return Ok(text.to_string()) };
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("[plugins.\"{id}\"]"));
            lines.push(format!("{key} = {v}"));
        }
    }
    let out = lines.join("\n") + "\n";
    let back = parse_decl(&out).ok();
    let mut expect = current.get(id).cloned().unwrap_or_default();
    set_field(&mut expect, key, value);
    if back.as_ref().and_then(|b| b.get(id)) == Some(&expect) {
        return Ok(out);
    }
    Ok(rewrite_block(text, id, &expect))
}

fn set_field(w: &mut Want, key: &str, value: Option<&toml::Value>) {
    let s = |v: Option<&toml::Value>| v.and_then(toml::Value::as_str).unwrap_or_default().to_string();
    match key {
        "source" => w.source = s(value),
        "ref" => w.rev = s(value),
        "enabled" => w.enabled = value.and_then(toml::Value::as_bool),
        "agents" => {
            w.agents = value
                .and_then(toml::Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default()
        }
        "settings" => w.settings = value.and_then(toml::Value::as_table).cloned().unwrap_or_default(),
        "tools" => w.tools = value.and_then(toml::Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()),
        "keys" | "hotkeys" => {
            let map = value
                .and_then(toml::Value::as_table)
                .map(|t| t.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
                .unwrap_or_default();
            if key == "keys" { w.keys = map } else { w.hotkeys = map }
        }
        _ => {}
    }
}

/// Replaces the whole section with what `w` says (comments in the section are lost, other sections untouched)
fn rewrite_block(text: &str, id: &str, w: &Want) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let block = render(id, w);
    match lines.iter().position(|l| is_head(l, id)) {
        Some(h) => {
            let end = block_end(&lines, h);
            lines.splice(h..end, block.lines().map(str::to_string));
        }
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.extend(block.lines().map(str::to_string));
        }
    }
    lines.join("\n").trim_end().to_string() + "\n"
}

fn render(id: &str, w: &Want) -> String {
    let mut out = format!("[plugins.\"{id}\"]\n");
    let mut put = |k: &str, v: toml::Value| out.push_str(&format!("{k} = {v}\n"));
    if !w.source.is_empty() {
        put("source", w.source.clone().into());
    }
    if !w.rev.is_empty() {
        put("ref", w.rev.clone().into());
    }
    if let Some(e) = w.enabled {
        put("enabled", e.into());
    }
    if !w.agents.is_empty() {
        put("agents", toml::Value::Array(w.agents.iter().map(|a| a.clone().into()).collect()));
    }
    if !w.settings.is_empty() {
        put("settings", toml::Value::Table(w.settings.clone()));
    }
    if let Some(t) = &w.tools {
        put("tools", toml::Value::Array(t.iter().map(|x| x.clone().into()).collect()));
    }
    for (name, map) in [("keys", &w.keys), ("hotkeys", &w.hotkeys)] {
        if !map.is_empty() {
            put(name, toml::Value::Table(map.iter().map(|(k, v)| (k.clone(), v.clone().into())).collect()));
        }
    }
    out
}

/// Removes one plugin's section (with the comments right above its header)
pub(crate) fn drop_block(text: &str, id: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if let Some(h) = lines.iter().position(|l| is_head(l, id)) {
        let end = block_end(&lines, h);
        let start = (0..h).rev().take_while(|&i| lines[i].trim_start().starts_with('#')).last().unwrap_or(h);
        lines.drain(start..end);
    }
    let out = lines.join("\n");
    let out = out.trim_matches('\n');
    if out.is_empty() { String::new() } else { format!("{out}\n") }
}

const HEADER: &str = "\
# 要哪些插件(aca help plugin)。改了之后 aca plugin sync 让本机跟它一致;命令行、界面里装 / 删 / 开停也会改这里。
";

/// Readable only by you: plugin settings hold secrets (webhook URLs, server tokens)
fn write_decl(text: &str) -> Res<()> {
    paths::write_private(&decl_file(), text).map_err(|e| tr!("无法写入 plugins.toml:{e}", "can't write plugins.toml: {e}"))
}

/// No plugins.toml yet: write one from the registry (the ones installed earlier with link / install)
pub fn ensure() -> Res<()> {
    if decl_file().exists() {
        return Ok(());
    }
    let mut text = HEADER.to_string();
    // Your own one (~/.config/aca/user/) doesn't need to be written here
    for e in super::registry().into_iter().filter(|e| e.source != super::user::SOURCE) {
        let w = Want {
            source: match e.source.as_str() {
                "" => e.path.clone(),
                super::first_party::SOURCE => String::new(),
                s => s.split('#').next().unwrap_or("").to_string(),
            },
            rev: e.source.split_once('#').map(|(_, r)| r.to_string()).unwrap_or_default(),
            enabled: (!e.enabled).then_some(false),
            ..Default::default()
        };
        text.push('\n');
        text.push_str(&render(&e.id, &w));
    }
    write_decl(&text)
}

/// Changes one key of a plugin in plugins.toml (writes one from the registry first if the file doesn't exist)
pub fn set(id: &str, key: &str, value: Option<toml::Value>) -> Res<()> {
    ensure()?;
    let text = std::fs::read_to_string(decl_file()).unwrap_or_default();
    write_decl(&edit(&text, id, key, value.as_ref())?)
}

/// Makes sure plugins.toml has a section for this plugin (adds a bare header if not: plugins shipped with aca need no source)
pub fn declare(id: &str) -> Res<()> {
    ensure()?;
    let text = std::fs::read_to_string(decl_file()).unwrap_or_default();
    if text.lines().any(|l| is_head(l, id)) {
        return Ok(());
    }
    write_decl(&rewrite_block(&text, id, &Want::default()))
}

/// The line of this plugin's section in plugins.toml (from 1; no section = the last line): where the editor opens
pub fn line_of(id: &str) -> usize {
    let text = std::fs::read_to_string(decl_file()).unwrap_or_default();
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    lines.iter().position(|l| is_head(l, id)).map_or(lines.len().max(1), |i| i + 1)
}

/// Removes a plugin from plugins.toml
pub fn forget(id: &str) -> Res<()> {
    ensure()?;
    let text = std::fs::read_to_string(decl_file()).unwrap_or_default();
    write_decl(&drop_block(&text, id))
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Lock {
    plugins: BTreeMap<String, Locked>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
struct Locked {
    commit: String,
}

/// plugins.lock: which commit each plugin installed from git is at
pub fn locked() -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(lock_file()).unwrap_or_default();
    toml::from_str::<Lock>(&text).map(|l| l.plugins.into_iter().map(|(k, v)| (k, v.commit)).collect()).unwrap_or_default()
}

/// Records (or removes: None) a plugin's commit
pub fn lock(id: &str, commit: Option<&str>) -> Res<()> {
    let mut all = locked();
    match commit {
        Some(c) => all.insert(id.to_string(), c.to_string()),
        None => all.remove(id),
    };
    let l = Lock { plugins: all.into_iter().map(|(k, commit)| (k, Locked { commit })).collect() };
    let text = tr!(
        "# aca plugin sync 按这里记录的提交安装;aca plugin update 会更新它。无需手动修改\n{}",
        "# aca plugin sync installs the commits recorded here; aca plugin update updates them. No need to edit by hand\n{}",
        toml::to_string(&l).unwrap_or_default()
    );
    paths::write_atomic(&lock_file(), &text).map_err(|e| tr!("无法写入 plugins.lock:{e}", "can't write plugins.lock: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "\
# 我的插件
[plugins.\"aca.delegate\"]
source = \"~/src/delegate\"  # 本地的
settings = { max_continue = 3 }

# 推送
[plugins.bark]
source = \"owner/bark\"
";

    #[test]
    fn one_key_changes_and_everything_else_stays() {
        let out = edit(TEXT, "aca.delegate", "enabled", Some(&false.into())).unwrap();
        assert!(out.contains("# 我的插件") && out.contains("# 本地的") && out.contains("# 推送"), "{out}");
        let w = parse_decl(&out).unwrap();
        assert_eq!((w["aca.delegate"].on(), w["bark"].on()), (false, true));
        assert_eq!(w["aca.delegate"].settings.get("max_continue"), Some(&toml::Value::Integer(3)));
        // An existing key is replaced in place; a key is removed
        let out = edit(&out, "aca.delegate", "enabled", Some(&true.into())).unwrap();
        assert_eq!(out.matches("enabled").count(), 1);
        let out = edit(&out, "aca.delegate", "enabled", None).unwrap();
        assert!(!out.contains("enabled"));
        // Unquoted headers work too; a plugin that isn't there is added at the end
        let out = edit(TEXT, "bark", "ref", Some(&"v2".into())).unwrap();
        assert_eq!(parse_decl(&out).unwrap()["bark"].rev, "v2");
        let out = edit(TEXT, "new.one", "source", Some(&"x/y".into())).unwrap();
        assert!(out.ends_with("[plugins.\"new.one\"]\nsource = \"x/y\"\n"), "{out}");
    }

    #[test]
    fn a_value_spread_over_lines_rewrites_its_block_and_keeps_the_rest() {
        let text = "[plugins.a]\nagents = [\n  \"claude\",\n]\n\n# 别的\n[plugins.b]\nsource = \"x\"\n";
        let out = edit(text, "a", "agents", Some(&toml::Value::Array(vec!["codex".into()]))).unwrap();
        let w = parse_decl(&out).unwrap();
        assert_eq!((w["a"].agents.clone(), w["b"].source.as_str()), (vec!["codex".to_string()], "x"));
        assert!(out.contains("# 别的"), "{out}");
    }

    #[test]
    fn tools_and_keys_are_written_and_read_back() {
        let out = edit(TEXT, "bark", "tools", Some(&toml::Value::Array(vec!["list".into()]))).unwrap();
        let keys: toml::Table = toml::from_str("tasks = \"T\"\nrun = \"\"").unwrap();
        let out = edit(&out, "bark", "keys", Some(&toml::Value::Table(keys))).unwrap();
        let w = parse_decl(&out).unwrap();
        assert_eq!(w["bark"].tools, Some(vec!["list".to_string()]));
        assert_eq!((w["bark"].keys["tasks"].as_str(), w["bark"].keys["run"].as_str()), ("T", ""));
        let hot: toml::Table = toml::from_str("pad = \"M-n\"").unwrap();
        let out = edit(&out, "bark", "hotkeys", Some(&toml::Value::Table(hot))).unwrap();
        let w = parse_decl(&out).unwrap();
        assert_eq!((w["bark"].hotkeys["pad"].as_str(), w["bark"].keys.len()), ("M-n", 2));
        // Giving none (an empty array) is different from not writing it (follow the manifest)
        let out = edit(&out, "bark", "tools", Some(&toml::Value::Array(vec![]))).unwrap();
        assert_eq!(parse_decl(&out).unwrap()["bark"].tools, Some(vec![]));
        let out = edit(&out, "bark", "tools", None).unwrap();
        assert_eq!(parse_decl(&out).unwrap()["bark"].tools, None);
        assert!(out.contains("# 推送"), "{out}");
        // Kept when the whole section is rewritten too
        let w = Want {
            tools: Some(vec!["a".into()]),
            keys: [("v".to_string(), "V".to_string())].into(),
            hotkeys: [("p".to_string(), "C-]".to_string())].into(),
            ..Default::default()
        };
        assert_eq!(parse_decl(&render("p", &w)).unwrap()["p"], w);
    }

    #[test]
    fn dropping_a_plugin_takes_its_comment_along() {
        let out = drop_block(TEXT, "bark");
        assert!(!out.contains("推送") && !out.contains("bark") && out.contains("aca.delegate"), "{out}");
        assert_eq!(drop_block("[plugins.a]\nsource = \"x\"\n", "a"), "");
    }

    #[test]
    fn who_a_plugin_reaches() {
        let w = |a: &[&str]| Want { agents: a.iter().map(|s| s.to_string()).collect(), ..Default::default() };
        assert!(w(&[]).reaches("codex", "codex", "labs"));
        assert!(w(&["claude:*"]).reaches("claude", "claude", "sub") && !w(&["claude:*"]).reaches("codex", "codex", ""));
        assert!(w(&["codex:labs"]).reaches("codex", "codex", "labs") && !w(&["codex:labs"]).reaches("codex", "codex", "other"));
        assert!(w(&["claude"]).reaches("claude", "claude", "sub"));
        // A tool you named yourself (ccx starts as claude): naming the tool or claude both count
        assert!(w(&["ccx"]).reaches("ccx", "claude", "") && w(&["claude:*"]).reaches("ccx", "claude", ""));
    }
}
