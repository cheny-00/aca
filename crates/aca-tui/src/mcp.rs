//! `M`: view / change the MCPs attached to a session.
//!
//! While the gateway plugin is on, the top rows are the servers this session sees through the gateway (see
//! `aca_core::gateway::seen_by`): space turns one off / on for this session only, ⏎ applies it on the spot (Claude re-fetches
//! the tool list right away, Codex after a restart). `[mcps.*]` already served through the gateway aren't offered again
//! under 「项目」 (project).
//!
//! Only the 「项目」 (project) layer of a Claude session can be changed: the catalog is `[mcps.<name>]` in config.toml (apart
//! from `description`, the other keys are exactly the entry written into `.mcp.json`); attaching / detaching edits the
//! `.mcp.json` in the session's registered directory, effective after a restart. The rest is listed, not changed:
//! - Global (top-level mcpServers in the account dir's `.claude.json`): shared by several sessions; read from the dir the
//!   session actually uses
//! - Codex: all in `[mcp_servers]` of `$CODEX_HOME/config.toml`; every session on the same CODEX_HOME changes with it
//!
//! `.mcp.json` is created here in the project dir: when the repo has none, attaching also writes it into the local
//! `.git/info/exclude` (this machine only, not committed), so `git add -A` doesn't pull it into a commit; once everything is
//! detached, a `.mcp.json` left with only an empty table is deleted.

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
    /// Under the gateway and visible to this session: space turns it off / on for this session only
    Gateway,
    /// In the catalog (`[mcps.*]`): space attaches / detaches (writes .mcp.json)
    Project,
    /// In .mcp.json but not in the catalog: listed only
    Extra,
    /// In the account dir (Claude's .claude.json / Codex's config.toml): listed only
    Global,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub name: String,
    pub desc: String,
    pub kind: Kind,
    /// Attached now
    pub on: bool,
    /// What's wanted after space presses; applying touches only the ones that differ from on
    pub want: bool,
}

/// The rows: editable ones first (in catalog order), then extra ones in .mcp.json, global ones last.
/// One already global gets no 「项目」 (project) row: attaching it in both places starts the same one twice
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

/// The ones in the gateway group this session turns off (all of them, not only the changed ones)
pub fn gateway_off(rows: &[Row]) -> Vec<String> {
    rows.iter().filter(|r| r.kind == Kind::Gateway && !r.want).map(|r| r.name.clone()).collect()
}

/// The ones to attach, the ones to detach
pub fn changes(rows: &[Row]) -> (Vec<String>, Vec<String>) {
    let pick = |on: bool| rows.iter().filter(|r| r.kind == Kind::Project && r.want == on && r.on != on).map(|r| r.name.clone()).collect();
    (pick(true), pick(false))
}

/// The `M` popup: one MCP per row, "[x] name  project / global  description", space attaches / detaches, ⏎ applies.
/// Editable ones get a box, read-only ones a dot; names changed but not applied yet are in the accent color
pub struct View {
    t: Target,
    /// Where .mcp.json is written (the session's registered dir)
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
                    // The gateway group changes on the spot: write this session's list, and tell the running gateway so
                    // the tool list is re-fetched. Claude re-fetches it right away, Codex needs a restart
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

/// What was read: the config dir the session actually uses (empty if it can't be read), the rows
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    pub home: String,
    pub rows: Vec<Row>,
}

/// Read when M is pressed: all local files
pub fn load(s: &Session) -> Info {
    let home = home_of(s);
    let (project, global) = match s.agent {
        Agent::Claude => {
            // Claude without CLAUDE_CONFIG_DIR uses ~/.claude.json (not inside ~/.claude)
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
        // Ones already offered through the gateway aren't offered again under 「项目」 (project)
        let served = aca_core::gateway::settings().servers;
        cat.retain(|(n, _)| !served.contains(n));
    }
    gw.extend(rows(s.agent, &cat, &project, &global));
    Info { rows: gw, home }
}

/// The servers this session sees through the gateway; turned-off ones have want / on both false
fn gateway_rows(session: &str, cat: &[(String, String)]) -> Vec<Row> {
    aca_core::gateway::seen_by(session)
        .into_iter()
        .map(|(spec, off)| {
            let desc = match (&spec.plugin, spec.tools) {
                // The name already comes from the plugin id, so it isn't written again
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

/// The catalog: config.toml's `[mcps.<name>]`, as (name, description), sorted by name
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

/// What this catalog entry looks like in `.mcp.json`: the keys of `[mcps.<name>]` other than `description` and `scope`
/// (meant for the gateway) are carried over as is
fn entry_of(t: &toml::Table, name: &str) -> Option<serde_json::Value> {
    let mut def = t.get("mcps")?.get(name)?.as_table()?.clone();
    def.remove("description");
    def.remove("scope");
    serde_json::to_value(def).ok()
}

/// `.mcp.json` after attaching on and detaching off (other keys and other MCPs left as they are). A name not in the catalog
/// goes in Err
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

/// ⏎ pressed in `M`: edit `.mcp.json` under dir, then tidy up (see `tidy`)
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

/// The mcpServers names in a JSON file (.mcp.json / .claude.json), empty if unreadable
fn json_servers(p: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(p) else { return vec![] };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return vec![] };
    let mut names: Vec<String> = v.get("mcpServers").and_then(|m| m.as_object()).map(|o| o.keys().cloned().collect()).unwrap_or_default();
    names.sort();
    names
}

/// The names of `[mcp_servers.<name>]` in Codex's config.toml
fn toml_servers(p: &Path) -> Vec<String> {
    let t: toml::Table = std::fs::read_to_string(p).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    let mut names: Vec<String> = t.get("mcp_servers").and_then(|m| m.as_table()).map(|m| m.keys().cloned().collect()).unwrap_or_default();
    names.sort();
    names
}

/// The config dir the session actually uses. Running: CLAUDE_CONFIG_DIR / CODEX_HOME in the agent process (the same as the
/// `t` temporary terminal gets); stopped: the registered account's `home`. Can't tell = empty (each family's default dir)
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

/// The repo has this file (under version control): leave it alone, and don't write it into exclude
fn tracked(dir: &str, file: &str) -> bool {
    git(dir, &["ls-files", "--error-unmatch", "--", file]).is_some()
}

/// Tidy the project dir after attaching / detaching: if something was attached, write .mcp.json into the local exclude;
/// delete it if only an empty table is left; also delete an empty lock file (`.mcp.json.lock`, left by other tools)
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

/// Write .mcp.json (and the lock file) into this repo's local .git/info/exclude. Ones already ignored (global gitignore, the
/// repo's .gitignore) aren't written; the full path from the repo root is written, so a session registered in a subdirectory
/// only ignores that spot
fn exclude(dir: &str) {
    if tracked(dir, ".mcp.json") {
        return;
    }
    // check-ignore -q: exit code 0 when ignored
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

/// The lines exclude still lacks (existing ones aren't written again)
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
                // One already global gets no 「项目」 (project) row
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
        r[0].want = true; // alpha attached
        r[1].want = false; // beta detached
        r[2].want = false; // a global one can't change; flipping it doesn't count
        assert_eq!(changes(&r), (vec!["alpha".to_string()], vec!["beta".to_string()]));
    }

    #[test]
    fn stopped_sessions_find_their_home_in_the_account() {
        let cfg = aca_core::config::parse(
            "[accounts.claude]\nteam = \"/acct/team/\"\n[accounts.codex]\ncx = { home = \"/homes/cx\", args = [\"-p\", \"x\"] }\n",
        );
        assert_eq!(home_from_config(&cfg, "claude", "team"), "/acct/team");
        assert_eq!(home_from_config(&cfg, "codex", "cx"), "/homes/cx");
        // An account given directly as a dir
        assert_eq!(home_from_config(&cfg, "claude", "/h/.x"), "/h/.x");
        // No account, an unknown account, opened as shell: each family's default
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
        // No file yet: created
        let got = applied("", &cat, &["web".into()], &[]).unwrap();
        assert_eq!(v(&got), serde_json::json!({"mcpServers": {"web": {"command": "npx", "args": ["-y", "web-mcp"], "env": {"KEY": "k"}}}}));
        assert!(got.ends_with("}\n"));
        // Hand-written MCPs and other keys stay; detached ones are removed
        let had = r#"{"other": 1, "mcpServers": {"own": {"command": "x"}, "web": {"command": "old"}}}"#;
        let got = applied(had, &cat, &["docs".into()], &["web".into()]).unwrap();
        assert_eq!(
            v(&got),
            serde_json::json!({"other": 1, "mcpServers": {"own": {"command": "x"}, "docs": {"type": "http", "url": "https://x/mcp"}}})
        );
        // Not in the catalog, or a broken file: nothing touched, and it says why
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
        // Still attached / with other keys: not deleted
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
            return; // no git, no test
        }
        // Unaffected by this machine's global gitignore
        git(s, &["config", "core.excludesFile", "/dev/null"]).unwrap();
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let sub = d.join("sub");
        std::fs::write(sub.join(".mcp.json"), r#"{"mcpServers":{"a":{}}}"#).unwrap();
        tidy(sub.to_str().unwrap(), true);
        let ex = std::fs::read_to_string(d.join(".git/info/exclude")).unwrap();
        assert!(ex.contains("/sub/.mcp.json\n") && ex.contains("/sub/.mcp.json.lock\n"), "{ex}");
        assert_eq!(git(s, &["status", "--porcelain"]).unwrap(), "");
        // Attaching again doesn't write it twice
        tidy(sub.to_str().unwrap(), true);
        assert_eq!(std::fs::read_to_string(d.join(".git/info/exclude")).unwrap(), ex);
        // Already in the repo: not deleted even when emptied
        std::fs::write(d.join(".mcp.json"), "{\"mcpServers\":{}}").unwrap();
        git(s, &["add", ".mcp.json"]).unwrap();
        tidy(s, false);
        assert!(d.join(".mcp.json").exists());
        // Already ignored by the repo's .gitignore: not written into exclude
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
