//! `~/.config/aca/plugins.toml`:要哪些插件,像 vim-plug 列在 vimrc 里。可以跟 dotfiles 一起放,换台机器 `aca plugin sync` 一条。
//!
//! ```toml
//! [plugins."aca.delegate"]
//! source = "~/src/aca/plugins/delegate"   # 本地目录 = 链接;git 地址 / owner/repo = 克隆
//! agents = ["claude:*", "codex:work"]    # 给哪几个「工具[:账号]」带东西;不写 = 清单里给的那几家全给
//! settings = { max_continue = 3 }        # 盖过清单里的默认值
//! tools = ["dispatch"]                   # 哪些能力给 agent 当工具;不写 = 清单里写 agent 的那些(写 agent? 的要点名)
//! keys = { tasks = "T", run = "" }       # : 菜单里的键:能力 / 窗格 / 视图 → 键,"" = 不要键;不写的照清单
//! hotkeys = { pad = "M-n" }              # 会话里面的热键:窗格 → tmux 的键名,"" = 不要;不写的照清单
//!
//! [plugins."owner/gh-glance"]
//! source = "owner/gh-glance"
//! ref = "v1.2"                           # 钉在一个标签 / 分支 / 提交上
//! enabled = false                        # 先停着
//! ```
//!
//! `plugins.lock` 记 git 装的每个插件现在是哪个提交:`sync` 照它装(换机器装出来一样),`update` 拉新的后改它。
//!
//! 命令行和界面里的装、删、开停都顺手改这份文件(只改那一个插件的那几行,别的、注释原样留着);
//! 你手改了它,`aca plugin sync` 让本机跟它一致。还没有这个文件时,第一次用到就照现在登记的插件写一份。

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
    /// 给哪几个「工具[:账号]」带东西:`claude`、`claude:*`(这一家的哪个账号都行)、`codex:work`;不写 = 全给
    pub agents: Vec<String>,
    pub settings: toml::Table,
    /// 哪些能力给 agent 当工具(能力 id);None = 照清单(写 `agent` 的给,`agent?` 的不给)
    pub tools: Option<Vec<String>>,
    /// `:` 菜单里的键:能力 / 窗格 / 视图的 id → 一个字符;空字符串 = 不要键。没写的照清单的 `[[keys]]`
    pub keys: BTreeMap<String, String>,
    /// 会话里面的热键:窗格的 id → tmux 的键名;空字符串 = 不要。没写的照清单的 `[[hotkeys]]`
    pub hotkeys: BTreeMap<String, String>,
}

impl Want {
    pub fn on(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// 这个会话带不带:`tool` 是会话的工具名(`aca new` 后面那个),`kind` 是它按哪一家起的(claude / codex),
    /// 两个都认;`account` 是账号名
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

/// 读 plugins.toml。没有这个文件 = 空的;写坏了 = Err(说哪不对)
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

/// 一个插件在 plugins.toml 里的那一段;没写 / 文件坏了 = 默认的(开着、全给、没有设置)
pub fn want(id: &str) -> Want {
    wants().ok().and_then(|mut w| w.remove(id)).unwrap_or_default()
}

/// 交给插件命令的设置:清单里的默认值,plugins.toml 里写了的盖上去
pub fn settings(id: &str, m: &super::Manifest) -> toml::Table {
    let mut all = m.defaults();
    all.extend(want(id).settings);
    all
}

/// `[plugins."<id>"]` 这一段的表头(id 里没有点、只有字母数字 _ - 的也认不带引号的写法)
fn is_head(line: &str, id: &str) -> bool {
    let bare = line.split('#').next().unwrap_or("").trim();
    bare == format!("[plugins.\"{id}\"]")
        || (id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') && bare == format!("[plugins.{id}]"))
}

fn is_any_head(line: &str) -> bool {
    line.trim_start().starts_with('[')
}

/// 表头在 `h` 的那一段到哪为止(不含):下一个表头前面,紧挨着它的注释、空行算下一段的
fn block_end(lines: &[String], h: usize) -> usize {
    let mut end = lines[h + 1..].iter().position(|l| is_any_head(l)).map_or(lines.len(), |i| h + 1 + i);
    if end < lines.len() {
        while end > h + 1 && (lines[end - 1].trim().is_empty() || lines[end - 1].trim_start().starts_with('#')) {
            end -= 1;
        }
    }
    end
}

/// 把一个插件的一项改成 value(None = 删掉这项),别的行、注释原样留着;没有这一段就在末尾加。
/// 改完读回来对不上(值写成了好几行之类),这一段整个照现在的值重写
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
                    // 放在这一段最后一个非空行后面
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

/// 这一段整个换成 `w` 的样子(段里的注释没了,别的段不动)
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

/// 删掉一个插件的那一段(连同紧挨在表头上面的注释)
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

/// 只有自己能读:插件的设置里有口令(webhook 地址、server 的 token)
fn write_decl(text: &str) -> Res<()> {
    paths::write_private(&decl_file(), text).map_err(|e| tr!("无法写入 plugins.toml:{e}", "can't write plugins.toml: {e}"))
}

/// 还没有 plugins.toml:照登记表写一份(之前用 link / install 装的那些)
pub fn ensure() -> Res<()> {
    if decl_file().exists() {
        return Ok(());
    }
    let mut text = HEADER.to_string();
    // 你自己的那份(~/.config/aca/user/)不用写在这里
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

/// 改 plugins.toml 里一个插件的一项(没有这个文件先照登记表写一份)
pub fn set(id: &str, key: &str, value: Option<toml::Value>) -> Res<()> {
    ensure()?;
    let text = std::fs::read_to_string(decl_file()).unwrap_or_default();
    write_decl(&edit(&text, id, key, value.as_ref())?)
}

/// plugins.toml 里有这个插件的一段(没有就加一个光秃秃的表头:aca 自带的不用写 source)
pub fn declare(id: &str) -> Res<()> {
    ensure()?;
    let text = std::fs::read_to_string(decl_file()).unwrap_or_default();
    if text.lines().any(|l| is_head(l, id)) {
        return Ok(());
    }
    write_decl(&rewrite_block(&text, id, &Want::default()))
}

/// 这个插件那一段在 plugins.toml 的第几行(从 1 数;没有这一段 = 最后一行):编辑器打开时停在这
pub fn line_of(id: &str) -> usize {
    let text = std::fs::read_to_string(decl_file()).unwrap_or_default();
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    lines.iter().position(|l| is_head(l, id)).map_or(lines.len().max(1), |i| i + 1)
}

/// 从 plugins.toml 里删掉一个插件
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

/// plugins.lock:git 装的插件各是哪个提交
pub fn locked() -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(lock_file()).unwrap_or_default();
    toml::from_str::<Lock>(&text).map(|l| l.plugins.into_iter().map(|(k, v)| (k, v.commit)).collect()).unwrap_or_default()
}

/// 记下(或者删掉:None)一个插件的提交
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
        // 已有的那项就地换;去掉一项
        let out = edit(&out, "aca.delegate", "enabled", Some(&true.into())).unwrap();
        assert_eq!(out.matches("enabled").count(), 1);
        let out = edit(&out, "aca.delegate", "enabled", None).unwrap();
        assert!(!out.contains("enabled"));
        // 不带引号的表头也认;没有的插件加在末尾
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
        // 一个都不给(空数组)和没写(照清单)不一样
        let out = edit(&out, "bark", "tools", Some(&toml::Value::Array(vec![]))).unwrap();
        assert_eq!(parse_decl(&out).unwrap()["bark"].tools, Some(vec![]));
        let out = edit(&out, "bark", "tools", None).unwrap();
        assert_eq!(parse_decl(&out).unwrap()["bark"].tools, None);
        assert!(out.contains("# 推送"), "{out}");
        // 整段重写时也带着
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
        // 自己起名的工具(ccx 按 claude 起):写工具名、写 claude 都算
        assert!(w(&["ccx"]).reaches("ccx", "claude", "") && w(&["claude:*"]).reaches("ccx", "claude", ""));
    }
}
