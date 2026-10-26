//! `aca plugin gateway import`:把各账号里散装的 MCP 收进 gateway。
//!
//! 读的地方:Claude 账号目录里 `.claude.json` 顶层的 `mcpServers`(没登记目录的默认账号是 `~/.claude.json`),
//! Codex 账号目录里 `config.toml` 的 `[mcp_servers.*]`(默认 `~/.codex`)。同名、写法一样的并成一个;写法不一样的、
//! HTTP 的(gateway 现在只接 stdio)标出来不收。
//!
//! 不加 `--apply` 只列;加了才写:aca 自己的 config.toml 末尾追加 `[mcps.<名字>]`(已经有的不动),再收进 gateway。
//! **账号里的那份原样留着**:gateway 开着时 agent 会两份都看到,确认没问题后你自己去账号里删(删不删、什么时候删你定)。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::paths::{self, Res};
use crate::{t, tr};

/// 找到的一个
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// 在哪几个账号里(`claude`、`claude:work`、`codex:work`)
    pub from: Vec<String>,
    /// 收不了的原因(HTTP 的、几处写得不一样);None = 能收
    pub problem: Option<String>,
    /// aca 的 `[mcps]` 里已经有这个名字(不再写,只收进 gateway)
    pub known: bool,
}

/// 一个账号里读出来的:(名字, 定义)
fn claude_servers(file: &Path) -> Vec<(String, serde_json::Value)> {
    let Ok(text) = std::fs::read_to_string(file) else { return vec![] };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return vec![] };
    v.get("mcpServers").and_then(|m| m.as_object()).map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default()
}

fn codex_servers(file: &Path) -> Vec<(String, serde_json::Value)> {
    let t: toml::Table = std::fs::read_to_string(file).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    t.get("mcp_servers")
        .and_then(|m| m.as_table())
        .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), serde_json::to_value(v).ok()?))).collect())
        .unwrap_or_default()
}

/// 一个 stdio 服务怎么起:(命令, 参数, 环境)
type Stdio = (String, Vec<String>, BTreeMap<String, String>);

/// 定义 → 怎么起;HTTP 的、没写命令的 Err
fn stdio(def: &serde_json::Value) -> Result<Stdio, String> {
    if def.get("url").is_some() || matches!(def.get("type").and_then(|t| t.as_str()), Some("http" | "sse" | "streamable-http")) {
        return Err(t!("是 HTTP 服务,gateway 目前只支持 stdio", "an HTTP server; the gateway takes stdio ones only").into());
    }
    let command = def.get("command").and_then(|c| c.as_str()).ok_or(t!("缺少 command", "no command"))?.to_string();
    // `./x`、`dir/x` 是相对那个账号自己的目录的(Codex 应用装的那种),gateway 里起不来;`npx` 这种按 PATH 找的没事
    if command.contains('/') && !command.starts_with('/') && !command.starts_with('~') {
        return Err(tr!(
            "命令是相对路径({command}),依赖该账号自己的目录,无法在 gateway 中启动",
            "the command is a relative path ({command}), tied to that account's own directory; it can't run under the gateway"
        ));
    }
    let args = def
        .get("args")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let env = def
        .get("env")
        .and_then(|e| e.as_object())
        .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
        .unwrap_or_default();
    Ok((command, args, env))
}

/// 要读的账号:(叫什么, 哪一家, 账号目录;None = 这一家的默认目录)
fn accounts(cfg: &crate::config::Config) -> Vec<(String, &'static str, Option<PathBuf>)> {
    let mut out = vec![("claude".to_string(), "claude", None), ("codex".to_string(), "codex", None)];
    for kind in ["claude", "codex"] {
        for (name, a) in cfg.accounts.get(kind).into_iter().flatten() {
            if !a.home.is_empty() {
                out.push((format!("{kind}:{name}"), kind, Some(PathBuf::from(paths::expand(&a.home)))));
            }
        }
    }
    out
}

/// 各账号里的 MCP,并好、标好
pub fn scan() -> Vec<Found> {
    let cfg = crate::config::load();
    let known = super::mcps_names();
    let home = paths::home();
    let mut seen: Vec<(String, Vec<(String, serde_json::Value)>)> = vec![];
    for (label, kind, dir) in accounts(&cfg) {
        let servers = match (kind, dir) {
            ("claude", None) => claude_servers(&home.join(".claude.json")),
            ("claude", Some(d)) => claude_servers(&d.join(".claude.json")),
            (_, None) => codex_servers(&home.join(".codex/config.toml")),
            (_, Some(d)) => codex_servers(&d.join("config.toml")),
        };
        seen.push((label, servers));
    }
    merge(seen, &known)
}

/// 同名的并起来(分开写好测)。几处写得不一样的标出来
pub fn merge(seen: Vec<(String, Vec<(String, serde_json::Value)>)>, known: &[String]) -> Vec<Found> {
    let mut out: Vec<Found> = vec![];
    for (label, servers) in seen {
        for (name, def) in servers {
            let parsed = stdio(&def);
            match out.iter_mut().find(|f| f.name == name) {
                Some(f) => {
                    if !f.from.contains(&label) {
                        f.from.push(label.clone());
                    }
                    let same = parsed.as_ref().is_ok_and(|(c, a, e)| (c, a, e) == (&f.command, &f.args, &f.env));
                    if f.problem.is_none() && !same {
                        f.problem =
                            Some(t!("各账号中的写法不一致,请统一后再导入", "accounts define it differently; make them agree first").into());
                    }
                }
                None => {
                    let mut f = Found { name: name.clone(), from: vec![label.clone()], known: known.contains(&name), ..Default::default() };
                    match parsed {
                        Ok((c, a, e)) => (f.command, f.args, f.env) = (c, a, e),
                        Err(e) => f.problem = Some(e),
                    }
                    out.push(f);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 一段 `[mcps.<名字>]`
fn block(f: &Found) -> String {
    let key =
        if f.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') { f.name.clone() } else { format!("\"{}\"", f.name) };
    let mut s = tr!("\n# 从 {} 导入(aca plugin gateway import)\n", "\n# taken in from {} (aca plugin gateway import)\n", f.from.join(", "));
    s.push_str(&format!("[mcps.{key}]\ncommand = {}\n", toml::Value::String(f.command.clone())));
    if !f.args.is_empty() {
        s.push_str(&format!("args = {}\n", toml::Value::Array(f.args.iter().cloned().map(toml::Value::String).collect())));
    }
    if !f.env.is_empty() {
        let t: toml::Table = f.env.iter().map(|(k, v)| (k.clone(), toml::Value::String(v.clone()))).collect();
        s.push_str(&format!("env = {}\n", toml::Value::Table(t)));
    }
    s
}

/// 收进来:没有的追加到 aca 的 config.toml,都收进 gateway。返回收了哪几个
pub fn apply(found: &[Found], only: &[String]) -> Res<Vec<String>> {
    let picked: Vec<&Found> = found.iter().filter(|f| f.problem.is_none() && (only.is_empty() || only.contains(&f.name))).collect();
    if let Some(n) = only.iter().find(|n| !found.iter().any(|f| &f.name == *n)) {
        return Err(tr!("账号中找不到 {n}", "{n} isn't in any account"));
    }
    let file = paths::config_dir().join("config.toml");
    let mut text = std::fs::read_to_string(&file).unwrap_or_default();
    let before = text.clone();
    for f in picked.iter().filter(|f| !f.known) {
        text.push_str(&block(f));
    }
    if text != before {
        // 写之前确认整份还读得懂,读不懂就不写
        text.parse::<toml::Table>()
            .map_err(|e| tr!("生成的 config.toml 无法解析,未写入:{e}", "the new config.toml wouldn't parse; not written: {e}"))?;
        paths::write_atomic(&file, &text).map_err(|e| tr!("无法写入 {}:{e}", "can't write {}: {e}", file.display()))?;
    }
    let mut done = vec![];
    for f in picked {
        super::set_served(&f.name, true)?;
        done.push(f.name.clone());
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_same_server_in_several_accounts_is_one_and_the_odd_ones_are_named() {
        let gh = json!({ "command": "npx", "args": ["-y", "gh-mcp"], "env": { "TOKEN": "s3cret" } });
        let seen = vec![
            (
                "claude".to_string(),
                vec![("github".to_string(), gh.clone()), ("web".to_string(), json!({ "type": "http", "url": "https://x" }))],
            ),
            ("claude:sub".to_string(), vec![("github".to_string(), gh.clone()), ("fs".to_string(), json!({ "command": "fs-mcp" }))]),
            ("codex:labs".to_string(), vec![("fs".to_string(), json!({ "command": "fs-mcp", "args": ["/"] }))]),
        ];
        let mut seen = seen;
        seen[2].1.push(("cu".to_string(), json!({ "command": "./Computer Use.app/x", "args": ["mcp"] })));
        let found = merge(seen, &["github".into()]);
        assert!(
            found
                .iter()
                .find(|f| f.name == "cu")
                .unwrap()
                .problem
                .as_deref()
                .is_some_and(|p| p.contains("相对路径") || p.contains("relative"))
        );
        let by = |n: &str| found.iter().find(|f| f.name == n).unwrap();
        assert_eq!(by("github").from, ["claude", "claude:sub"]);
        assert!(by("github").problem.is_none() && by("github").known);
        assert!(by("web").problem.as_deref().is_some_and(|p| p.contains("HTTP")));
        assert!(by("fs").problem.is_some() && by("fs").from == ["claude:sub", "codex:labs"]);
        let b = block(by("github"));
        assert!(b.contains("[mcps.github]") && b.contains("command = \"npx\"") && b.contains("TOKEN = \"s3cret\""), "{b}");
        assert!(format!("{}{b}", "[mcps.x]\ncommand = \"y\"\n").parse::<toml::Table>().is_ok());
    }
}
