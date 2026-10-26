//! `M`:看 / 改会话挂的 MCP。
//!
//! gateway 插件开着时,最上面是这个会话从 gateway 看得到的服务(见 `aca_core::gateway::seen_by`):空格只对这个会话关掉 /
//! 打开,⏎ 当场生效(Claude 马上重新拉工具列表,Codex 重启后)。已经收进 gateway 的 `[mcps.*]` 不再在「项目」里重复给。
//!
//! 能改的只有 Claude 会话的「项目」一层:目录是 config.toml 里的 `[mcps.<名字>]`(除了 `description`,
//! 别的键原样就是写进 `.mcp.json` 的那一项),挂上 / 卸下就是改会话登记目录的 `.mcp.json`,重启后生效。别的只列不改:
//! - 全局(账号目录 `.claude.json` 顶层的 mcpServers):几个会话共用,读会话实际用的那个目录
//! - Codex:都在 `$CODEX_HOME/config.toml` 的 `[mcp_servers]`,用同一个 CODEX_HOME 的会话全跟着变
//!
//! `.mcp.json` 是这里在项目目录里建的:仓库里没有它时,挂上的同时写进本地的 `.git/info/exclude`
//! (只在这台机器、不入库),免得被 `git add -A` 带进提交;全卸掉后删掉只剩空表的 `.mcp.json`。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use aca_core::{t, tr};

use crate::data::{Agent, Session};
use crate::ops::{Job, Outcome, Target};
use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// gateway 下面、这个会话看得到的:空格只对这个会话关掉 / 打开
    Gateway,
    /// 目录(`[mcps.*]`)里有:空格挂上 / 卸下(写 .mcp.json)
    Project,
    /// .mcp.json 里有、目录里没有:只列
    Extra,
    /// 账号目录里的(Claude 的 .claude.json / Codex 的 config.toml):只列
    Global,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub name: String,
    pub desc: String,
    pub kind: Kind,
    /// 现在挂着
    pub on: bool,
    /// 按了空格之后要的;应用时只动和 on 不一样的
    pub want: bool,
}

/// 各行:能改的在前(目录的顺序),再是 .mcp.json 里另有的,最后是全局的。
/// 全局已经有的不再给「项目」那一行:两处都挂等于同一个起两份
pub fn rows(agent: Agent, catalog: &[(String, String)], project: &[String], global: &[String]) -> Vec<Row> {
    let desc = |n: &str| catalog.iter().find(|(c, _)| c == n).map(|(_, d)| d.clone()).unwrap_or_default();
    let row = |name: &str, kind, on| Row { name: name.to_string(), desc: desc(name), kind, on, want: on };
    let mut out = vec![];
    if agent == Agent::Claude {
        for (n, _) in catalog.iter().filter(|(n, _)| !global.contains(n)) {
            out.push(row(n, Kind::Project, project.contains(n)));
        }
        for p in project.iter().filter(|p| !global.contains(p) && !catalog.iter().any(|(c, _)| c == *p)) {
            out.push(row(p, Kind::Extra, true));
        }
    }
    out.extend(global.iter().map(|g| row(g, Kind::Global, true)));
    out
}

/// gateway 那组里这个会话要关掉的(全部,不只是改了的)
pub fn gateway_off(rows: &[Row]) -> Vec<String> {
    rows.iter().filter(|r| r.kind == Kind::Gateway && !r.want).map(|r| r.name.clone()).collect()
}

/// 要挂上的、要卸下的
pub fn changes(rows: &[Row]) -> (Vec<String>, Vec<String>) {
    let pick = |on: bool| rows.iter().filter(|r| r.kind == Kind::Project && r.want == on && r.on != on).map(|r| r.name.clone()).collect();
    (pick(true), pick(false))
}

/// `M` 的弹窗:一行一个 MCP「[x] 名字  项目 / 全局  说明」,空格挂上 / 卸下,⏎ 应用。
/// 能改的是方框,只读的是圆点;改过还没应用的名字用强调色
pub struct View {
    t: Target,
    /// 写 .mcp.json 的地方(会话登记的目录)
    dir: String,
    rows: Vec<Row>,
    sel: usize,
}

impl View {
    pub fn new(t: Target, dir: String, rows: Vec<Row>) -> Self {
        View { t, dir, rows, sel: 0 }
    }
}

impl Overlay for View {
    fn width(&self) -> u16 {
        72
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        let edit = self.rows.iter().any(|r| matches!(r.kind, Kind::Project | Kind::Gateway));
        let title = format!("MCP · {}", self.t.title);
        let mut out = vec![
            kit::modal_title(&title, theme::accent(), kit::esc_hint(if edit { t!("取消", "cancel") } else { t!("关闭", "close") }), w),
            Line::raw(""),
        ];
        if self.rows.is_empty() {
            out.push(Line::styled(t!("暂无 MCP", "no MCPs"), kit::dim()));
        }
        let name_w = self.rows.iter().map(|r| r.name.width()).max().unwrap_or(0).min(16);
        for (i, r) in self.rows.iter().enumerate() {
            let (mark, tag) = match r.kind {
                Kind::Gateway => (if r.want { "[x]" } else { "[ ]" }, "gateway"),
                Kind::Project => (if r.want { "[x]" } else { "[ ]" }, t!("项目", "project")),
                Kind::Extra => (" · ", t!("项目", "project")),
                Kind::Global => (" · ", t!("全局", "global")),
            };
            let changed = r.want != r.on;
            let name_st = if changed { theme::accent().add_modifier(Modifier::BOLD) } else { Style::new().add_modifier(Modifier::BOLD) };
            let desc = kit::clip(&r.desc, w.saturating_sub(3 + 1 + name_w + 2 + tag.width() + 2)).trim_end().to_string();
            let line = Line::from(vec![
                Span::styled(mark, if matches!(r.kind, Kind::Project | Kind::Gateway) { Style::new() } else { kit::dim() }),
                Span::raw(" "),
                Span::styled(kit::clip(&r.name, name_w), name_st),
                Span::raw("  "),
                Span::styled(tag, kit::dim()),
                Span::raw("  "),
                Span::styled(desc, kit::dim()),
            ]);
            out.push(kit::picked(line, w, i == self.sel));
        }
        if edit {
            out.push(Line::raw(""));
            out.push(kit::keys_line(
                &[(t!("空格", "space"), t!("切换", "toggle"))],
                vec![kit::button(t!("⏎ 应用", "⏎ apply"), theme::pal().accent)],
                w,
            ));
        }
        out
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        let n = self.rows.len();
        match k.code {
            KeyCode::Down | KeyCode::Char('j') if n > 0 => self.sel = (self.sel + 1) % n,
            KeyCode::Up | KeyCode::Char('k') if n > 0 => self.sel = (self.sel + n - 1) % n,
            KeyCode::Char(' ') => match self.rows.get_mut(self.sel) {
                Some(r) if matches!(r.kind, Kind::Project | Kind::Gateway) => r.want = !r.want,
                Some(_) => fx.say(t!("此项只读", "this one is read-only")),
                None => {}
            },
            KeyCode::Enter => {
                let (on, off) = changes(&self.rows);
                let gw_changed = self.rows.iter().any(|r| r.kind == Kind::Gateway && r.want != r.on);
                if gw_changed {
                    // gateway 那组当场改:写这个会话的名单,在跑的 gateway 通知它重新拉工具列表
                    // Claude 马上重新拉工具列表,Codex 要重启
                    match aca_core::gateway::set_off(&self.t.id, &gateway_off(&self.rows)) {
                        Ok(()) => fx.say(t!("已修改 · Codex 重启后生效", "changed · Codex picks it up after a restart")),
                        Err(e) => fx.say(e),
                    }
                }
                if !on.is_empty() || !off.is_empty() {
                    fx.job = Some(Job::Mcp { t: self.t.clone(), dir: self.dir.clone(), on, off });
                } else if !gw_changed {
                    fx.say(t!("MCP 未修改", "MCPs unchanged"));
                }
                return Flow::Close;
            }
            KeyCode::Esc | KeyCode::Char('q') => return Flow::Close,
            _ => {}
        }
        Flow::Stay
    }
}

/// 读出来的:会话实际用的配置目录(读不出就空)、各行
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    pub home: String,
    pub rows: Vec<Row>,
}

/// 按 M 时读:都是本地文件
pub fn load(s: &Session) -> Info {
    let home = home_of(s);
    let (project, global) = match s.agent {
        Agent::Claude => {
            // 没设 CLAUDE_CONFIG_DIR 的 Claude 用的是 ~/.claude.json(不在 ~/.claude 里面)
            let cfg = if home.is_empty() { PathBuf::from(env_home()).join(".claude.json") } else { Path::new(&home).join(".claude.json") };
            (json_servers(&Path::new(&s.dir).join(".mcp.json")), json_servers(&cfg))
        }
        Agent::Codex => {
            let h = if home.is_empty() { format!("{}/.codex", env_home()) } else { home.clone() };
            (vec![], toml_servers(&Path::new(&h).join("config.toml")))
        }
        _ => (vec![], vec![]),
    };
    let mut cat = catalog();
    let mut gw = vec![];
    if aca_core::gateway::enabled() && !s.deck_id.is_empty() {
        gw = gateway_rows(&s.deck_id, &cat);
        // 已经经 gateway 给了的,不再在「项目」里给一份
        let served = aca_core::gateway::settings().servers;
        cat.retain(|(n, _)| !served.contains(n));
    }
    gw.extend(rows(s.agent, &cat, &project, &global));
    Info { rows: gw, home }
}

/// 这个会话从 gateway 看得到的服务,关掉的 want / on 都是 false
fn gateway_rows(session: &str, cat: &[(String, String)]) -> Vec<Row> {
    aca_core::gateway::seen_by(session)
        .into_iter()
        .map(|(spec, off)| {
            let desc = match (&spec.plugin, spec.tools) {
                // 名字已经是从插件 id 来的,不再写一遍
                (Some(_), _) => t!("来自插件", "from a plugin").to_string(),
                (None, true) => t!("插件提供给 agent 的工具", "plugin tools for agents").to_string(),
                (None, false) => {
                    cat.iter().find(|(n, _)| aca_core::plugin::mount::safe(n) == spec.name).map(|(_, d)| d.clone()).unwrap_or_default()
                }
            };
            Row { name: spec.name, desc, kind: Kind::Gateway, on: !off, want: !off }
        })
        .collect()
}

fn env_home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn expand(d: &str) -> String {
    match d.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", env_home()),
        None => d.to_string(),
    }
}

fn config() -> toml::Table {
    std::fs::read_to_string(crate::config::path()).ok().and_then(|t| t.parse().ok()).unwrap_or_default()
}

/// 目录:config.toml 的 `[mcps.<名>]`,(名字, 说明),按名字排
fn catalog() -> Vec<(String, String)> {
    catalog_of(&config())
}

fn catalog_of(t: &toml::Table) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = t
        .get("mcps")
        .and_then(|m| m.as_table())
        .into_iter()
        .flatten()
        .map(|(n, m)| (n.clone(), m.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string()))
        .collect();
    v.sort();
    v
}

/// 目录里这一项写进 `.mcp.json` 是什么样:`[mcps.<名>]` 里除了 `description`、`scope`(给 gateway 看的)的键原样搬过去
fn entry_of(t: &toml::Table, name: &str) -> Option<serde_json::Value> {
    let mut def = t.get("mcps")?.get(name)?.as_table()?.clone();
    def.remove("description");
    def.remove("scope");
    serde_json::to_value(def).ok()
}

/// 挂上 on、卸下 off 之后的 `.mcp.json`(别的键、别的 MCP 原样留着)。目录里没有的名字放在 Err 里
fn applied(text: &str, cat: &toml::Table, on: &[String], off: &[String]) -> Result<String, String> {
    let mut doc: serde_json::Value = if text.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(text).map_err(|e| tr!("无法解析 .mcp.json:{e}", "can't parse .mcp.json: {e}"))?
    };
    let root = doc.as_object_mut().ok_or(t!(".mcp.json 不是 JSON 对象", ".mcp.json isn't an object"))?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or(t!(".mcp.json 的 mcpServers 不是 JSON 对象", "mcpServers in .mcp.json isn't an object"))?;
    for n in on {
        let e = entry_of(cat, n).ok_or_else(|| tr!("config.toml 的 [mcps] 中没有 {n}", "no {n} under [mcps] in config.toml"))?;
        servers.insert(n.clone(), e);
    }
    for n in off {
        servers.remove(n);
    }
    let mut out = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    out.push('\n');
    Ok(out)
}

/// `M` 里按了 ⏎:改 dir 下的 `.mcp.json`,再收拾一下(见 `tidy`)
pub fn apply(dir: &str, on: &[String], off: &[String]) -> Outcome {
    let path = Path::new(dir).join(".mcp.json");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let new = match applied(&text, &config(), on, off) {
        Ok(t) => t,
        Err(e) => return Outcome::Failed(tr!("MCP 未修改:{e}", "MCPs unchanged: {e}")),
    };
    if let Err(e) = aca_core::paths::write_atomic(&path, &new) {
        return Outcome::Failed(tr!("MCP 未修改:无法写入 {}:{e}", "MCPs unchanged: can't write {}: {e}", path.display()));
    }
    tidy(dir, !on.is_empty());
    let mut parts = vec![];
    if !on.is_empty() {
        parts.push(tr!("已挂载 {}", "attached {}", on.join(t!("、", ", "))));
    }
    if !off.is_empty() {
        parts.push(tr!("已卸载 {}", "detached {}", off.join(t!("、", ", "))));
    }
    parts.push(t!("重启后生效", "takes effect after a restart").into());
    Outcome::Done(parts.join(" · "))
}

/// JSON 文件里 mcpServers 的名字(.mcp.json / .claude.json),读不了就空
fn json_servers(p: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(p) else { return vec![] };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return vec![] };
    let mut names: Vec<String> = v.get("mcpServers").and_then(|m| m.as_object()).map(|o| o.keys().cloned().collect()).unwrap_or_default();
    names.sort();
    names
}

/// Codex config.toml 里 `[mcp_servers.<名>]` 的名字
fn toml_servers(p: &Path) -> Vec<String> {
    let t: toml::Table = std::fs::read_to_string(p).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    let mut names: Vec<String> = t.get("mcp_servers").and_then(|m| m.as_table()).map(|m| m.keys().cloned().collect()).unwrap_or_default();
    names.sort();
    names
}

/// 会话实际用的配置目录。在跑的:agent 进程里的 CLAUDE_CONFIG_DIR / CODEX_HOME(和 t 终端带的一样);
/// 停着的:登记的账号那一层的 `home`。推不出 = 空(用各家的默认目录)
pub(crate) fn home_of(s: &Session) -> String {
    let key = if s.agent == Agent::Codex { "CODEX_HOME" } else { "CLAUDE_CONFIG_DIR" };
    if !s.pane.is_empty()
        && let Some((_, d)) = crate::data::account_env(Some(s), s.pid, s.agent).into_iter().find(|(k, _)| k == key)
    {
        return expand(&d).trim_end_matches('/').to_string();
    }
    home_from_config(&aca_core::config::load(), &s.tool, &s.account)
}

fn home_from_config(cfg: &aca_core::config::Config, tool: &str, account: &str) -> String {
    let Ok((agent, _)) = aca_core::agent::resolve(cfg, tool) else { return String::new() };
    aca_core::agent::account(cfg, &*agent, account).map(|a| a.home).unwrap_or_default()
}

fn git(dir: &str, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string())
}

/// 仓库里有这个文件(纳入版本管理的):别动它,也不往 exclude 里写
fn tracked(dir: &str, file: &str) -> bool {
    git(dir, &["ls-files", "--error-unmatch", "--", file]).is_some()
}

/// 挂 / 卸完收拾项目目录:挂上了就把 .mcp.json 写进本地 exclude;只剩空表就删掉;空的锁文件(`.mcp.json.lock`,别的工具留下的)也删掉
pub fn tidy(dir: &str, attached: bool) {
    let json = Path::new(dir).join(".mcp.json");
    if attached {
        exclude(dir);
    }
    let empty = std::fs::read_to_string(&json).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).is_some_and(|v| {
        v.as_object().is_some_and(|o| o.len() == 1 && o.get("mcpServers").and_then(|m| m.as_object()).is_some_and(|m| m.is_empty()))
    });
    if empty && !tracked(dir, ".mcp.json") {
        let _ = std::fs::remove_file(&json);
    }
    let lock = Path::new(dir).join(".mcp.json.lock");
    if std::fs::metadata(&lock).is_ok_and(|m| m.is_file() && m.len() == 0) && !tracked(dir, ".mcp.json.lock") {
        let _ = std::fs::remove_file(&lock);
    }
}

/// 把 .mcp.json(和锁文件)写进这个仓库本地的 .git/info/exclude。已经被忽略的(全局 gitignore、仓库的 .gitignore)不写;
/// 按仓库根写全路径:会话登记在子目录时只忽略那一处
fn exclude(dir: &str) {
    if tracked(dir, ".mcp.json") {
        return;
    }
    // check-ignore -q:被忽略时退出码 0
    let files: Vec<&str> =
        [".mcp.json", ".mcp.json.lock"].into_iter().filter(|f| git(dir, &["check-ignore", "-q", "--", f]).is_none()).collect();
    if files.is_empty() {
        return;
    }
    let (Some(path), Some(prefix)) = (git(dir, &["rev-parse", "--git-path", "info/exclude"]), git(dir, &["rev-parse", "--show-prefix"]))
    else {
        return;
    };
    let path = if Path::new(&path).is_absolute() { PathBuf::from(path) } else { Path::new(dir).join(path) };
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let add = exclude_lines(&text, &prefix, &files);
    if add.is_empty() {
        return;
    }
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let mut out = text;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&add);
    let _ = std::fs::write(&path, out);
}

/// exclude 里还缺的那几行(已有的不重复写)
fn exclude_lines(text: &str, prefix: &str, files: &[&str]) -> String {
    let want: Vec<String> = files.iter().map(|f| format!("/{prefix}{f}")).collect();
    let missing: Vec<&String> = want.iter().filter(|w| !text.lines().any(|l| l.trim() == w.as_str())).collect();
    if missing.is_empty() {
        return String::new();
    }
    let mut s = String::from("# aca: local MCP config\n");
    for m in missing {
        s.push_str(m);
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat() -> Vec<(String, String)> {
        vec![("alpha".into(), "A".into()), ("beta".into(), "B".into()), ("gamma".into(), String::new())]
    }

    fn names(rows: &[Row]) -> Vec<(String, Kind, bool)> {
        rows.iter().map(|r| (r.name.clone(), r.kind, r.on)).collect()
    }

    #[test]
    fn claude_rows_toggle_the_catalog_and_only_list_the_rest() {
        let r = rows(Agent::Claude, &cat(), &["beta".into(), "own".into()], &["gamma".into(), "zeta".into()]);
        assert_eq!(
            names(&r),
            [
                ("alpha".into(), Kind::Project, false),
                ("beta".into(), Kind::Project, true),
                ("own".into(), Kind::Extra, true),
                // 全局已有的不再给「项目」那一行
                ("gamma".into(), Kind::Global, true),
                ("zeta".into(), Kind::Global, true),
            ]
        );
        assert_eq!(r[1].desc, "B");
    }

    #[test]
    fn codex_rows_are_all_read_only() {
        let r = rows(Agent::Codex, &cat(), &["beta".into()], &["alpha".into()]);
        assert_eq!(names(&r), [("alpha".into(), Kind::Global, true)]);
    }

    #[test]
    fn only_flipped_project_rows_are_applied() {
        let mut r = rows(Agent::Claude, &cat(), &["beta".into()], &["gamma".into()]);
        assert_eq!(changes(&r), (vec![], vec![]));
        r[0].want = true; // alpha 挂上
        r[1].want = false; // beta 卸下
        r[2].want = false; // 全局的改不了,改了也不算
        assert_eq!(changes(&r), (vec!["alpha".to_string()], vec!["beta".to_string()]));
    }

    #[test]
    fn stopped_sessions_find_their_home_in_the_account() {
        let cfg = aca_core::config::parse(
            "[accounts.claude]\nteam = \"/acct/team/\"\n[accounts.codex]\ncx = { home = \"/homes/cx\", args = [\"-p\", \"x\"] }\n",
        );
        assert_eq!(home_from_config(&cfg, "claude", "team"), "/acct/team");
        assert_eq!(home_from_config(&cfg, "codex", "cx"), "/homes/cx");
        // 直接写目录的账号
        assert_eq!(home_from_config(&cfg, "claude", "/h/.x"), "/h/.x");
        // 没写账号、没有这个账号、开成 shell 的:用各家默认的
        assert_eq!(home_from_config(&cfg, "claude", ""), "");
        assert_eq!(home_from_config(&cfg, "claude", "nope"), "");
        assert_eq!(home_from_config(&cfg, "", ""), "");
    }

    #[test]
    fn attaching_writes_the_catalog_entry_and_keeps_everything_else() {
        let cat: toml::Table =
            "[mcps.web]\ncommand = \"npx\"\nargs = [\"-y\", \"web-mcp\"]\ndescription = \"搜网页\"\nenv = { KEY = \"k\" }\n\
                                [mcps.docs]\ntype = \"http\"\nurl = \"https://x/mcp\"\n"
                .parse()
                .unwrap();
        let v = |t: &str| serde_json::from_str::<serde_json::Value>(t).unwrap();
        // 还没有文件:建出来
        let got = applied("", &cat, &["web".into()], &[]).unwrap();
        assert_eq!(v(&got), serde_json::json!({"mcpServers": {"web": {"command": "npx", "args": ["-y", "web-mcp"], "env": {"KEY": "k"}}}}));
        assert!(got.ends_with("}\n"));
        // 自己手写的 MCP、别的键都留着;卸下的拿掉
        let had = r#"{"other": 1, "mcpServers": {"own": {"command": "x"}, "web": {"command": "old"}}}"#;
        let got = applied(had, &cat, &["docs".into()], &["web".into()]).unwrap();
        assert_eq!(
            v(&got),
            serde_json::json!({"other": 1, "mcpServers": {"own": {"command": "x"}, "docs": {"type": "http", "url": "https://x/mcp"}}})
        );
        // 目录里没有的、文件写坏了的:不动,说清楚
        assert!(applied(had, &cat, &["nope".into()], &[]).unwrap_err().contains("nope"));
        assert!(applied("{", &cat, &[], &["web".into()]).unwrap_err().contains("无法解析"));
        assert!(applied("[]", &cat, &[], &[]).unwrap_err().contains("不是 JSON 对象"));
    }

    #[test]
    fn catalog_reads_mcps_with_descriptions() {
        let t: toml::Table = "[mcps.zed]\ncommand = \"x\"\n[mcps.abc]\ncommand = \"y\"\ndescription = \"hi\"\n".parse().unwrap();
        assert_eq!(catalog_of(&t), [("abc".to_string(), "hi".to_string()), ("zed".to_string(), String::new())]);
    }

    #[test]
    fn exclude_adds_only_missing_lines_anchored_at_the_repo_root() {
        let both = [".mcp.json", ".mcp.json.lock"];
        let add = exclude_lines("*.log\n", "sub/dir/", &both);
        assert_eq!(add, "# aca: local MCP config\n/sub/dir/.mcp.json\n/sub/dir/.mcp.json.lock\n");
        assert_eq!(exclude_lines("/.mcp.json\n /.mcp.json.lock \n", "", &both), "");
        assert_eq!(exclude_lines("/.mcp.json\n", "", &both), "# aca: local MCP config\n/.mcp.json.lock\n");
        assert_eq!(exclude_lines("", "", &[".mcp.json.lock"]), "# aca: local MCP config\n/.mcp.json.lock\n");
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aca-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn tidy_removes_an_emptied_config_and_the_lock_but_keeps_real_ones() {
        let d = scratch("tidy");
        let s = d.to_str().unwrap();
        std::fs::write(d.join(".mcp.json"), "{\n  \"mcpServers\": {}\n}\n").unwrap();
        std::fs::write(d.join(".mcp.json.lock"), "").unwrap();
        tidy(s, false);
        assert!(!d.join(".mcp.json").exists() && !d.join(".mcp.json.lock").exists());
        // 还挂着的 / 带别的键的不删
        std::fs::write(d.join(".mcp.json"), r#"{"mcpServers":{"a":{}}}"#).unwrap();
        tidy(s, false);
        assert!(d.join(".mcp.json").exists());
        std::fs::write(d.join(".mcp.json"), r#"{"mcpServers":{},"other":1}"#).unwrap();
        tidy(s, false);
        assert!(d.join(".mcp.json").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn tidy_writes_the_local_exclude_in_a_repo_and_leaves_tracked_files_alone() {
        let d = scratch("git");
        let s = d.to_str().unwrap();
        if git(s, &["init", "-q"]).is_none() {
            return; // 没有 git 就不测
        }
        // 不受本机全局 gitignore 影响
        git(s, &["config", "core.excludesFile", "/dev/null"]).unwrap();
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let sub = d.join("sub");
        std::fs::write(sub.join(".mcp.json"), r#"{"mcpServers":{"a":{}}}"#).unwrap();
        tidy(sub.to_str().unwrap(), true);
        let ex = std::fs::read_to_string(d.join(".git/info/exclude")).unwrap();
        assert!(ex.contains("/sub/.mcp.json\n") && ex.contains("/sub/.mcp.json.lock\n"), "{ex}");
        assert_eq!(git(s, &["status", "--porcelain"]).unwrap(), "");
        // 再挂一次不重复写
        tidy(sub.to_str().unwrap(), true);
        assert_eq!(std::fs::read_to_string(d.join(".git/info/exclude")).unwrap(), ex);
        // 仓库里本来就有的:清空了也不删
        std::fs::write(d.join(".mcp.json"), "{\"mcpServers\":{}}").unwrap();
        git(s, &["add", ".mcp.json"]).unwrap();
        tidy(s, false);
        assert!(d.join(".mcp.json").exists());
        // 仓库的 .gitignore 已经忽略了的,不再往 exclude 里写
        let d2 = scratch("ignored");
        let s2 = d2.to_str().unwrap();
        git(s2, &["init", "-q"]).unwrap();
        git(s2, &["config", "core.excludesFile", "/dev/null"]).unwrap();
        std::fs::write(d2.join(".gitignore"), ".mcp.json\n").unwrap();
        std::fs::write(d2.join(".mcp.json"), r#"{"mcpServers":{"a":{}}}"#).unwrap();
        tidy(s2, true);
        let ex = std::fs::read_to_string(d2.join(".git/info/exclude")).unwrap();
        assert!(!ex.contains("/.mcp.json\n") && ex.contains("/.mcp.json.lock\n"), "{ex}");
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&d2);
    }
}
