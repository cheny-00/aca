//! 命令行。界面、插件、agent 自己都走这一套:能在界面里做的事,这里都有一条命令,`--json` 给程序读。
//! 不带参数的 `aca` 是界面(aca-tui);`cost` / `limits` / `theme` / `mounts` / `remote` 和界面共用代码,也在那边。
//! 每条命令怎么写、是干什么的,记在 `help.rs` 的表里(`aca help`)。

use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::json;

use aca_core::i18n::{self, Lang, Tr};
use aca_core::paths::{self, Res};
use aca_core::plugin::{self, Context};
use aca_core::runtime::{self, Token};
use aca_core::session::{self, New};
use aca_core::status::{self, Row};
use aca_core::store::Store;
use aca_core::{adopt, agent, answer, config, gateway, hook, import, inbox, t, tmux, tr};
use aca_tui::print;

use crate::help;

/// 有参数没给全(少了位置参数、选项后面没跟值):出错时在那句话后面补上这条命令的用法
static SHORT: AtomicBool = AtomicBool::new(false);

/// 最常要的几个位置参数(「要给会话 id」)
const ID: Tr = Tr::new("会话 id", "a session id");
const SUB: Tr = Tr::new("子命令", "a subcommand");
const GROUP_PATH: Tr = Tr::new("分组路径", "a group path");
const PLUGIN_ID: Tr = Tr::new("插件 id", "a plugin id");
const POS: Tr = Tr::new("位置", "a position");

struct Args(Vec<String>);

impl Args {
    /// 有没有这个开关(有就拿掉)
    fn flag(&mut self, name: &str) -> bool {
        match self.0.iter().position(|a| a == name) {
            Some(i) => {
                self.0.remove(i);
                true
            }
            None => false,
        }
    }

    /// 带值的参数(拿掉它和它的值)
    fn opt(&mut self, name: &str) -> Res<Option<String>> {
        let Some(i) = self.0.iter().position(|a| a == name) else { return Ok(None) };
        if i + 1 >= self.0.len() {
            SHORT.store(true, Ordering::Relaxed);
            return Err(tr!("{name} 后面要跟一个值", "{name} needs a value after it"));
        }
        self.0.remove(i);
        Ok(Some(self.0.remove(i)))
    }

    /// 下一个位置参数;what 是缺了它时说的「要给…」
    fn next(&mut self, what: Tr) -> Res<String> {
        if self.0.is_empty() {
            SHORT.store(true, Ordering::Relaxed);
            return Err(tr!("要给{what}", "missing {what}"));
        }
        Ok(self.0.remove(0))
    }

    fn maybe(&mut self) -> Option<String> {
        (!self.0.is_empty()).then(|| self.0.remove(0))
    }

    /// 会话 id;没写就是你所在的那个会话(见 [`here`])
    fn id(&mut self) -> Res<String> {
        match self.0.is_empty().then(here).flatten() {
            Some(id) => Ok(id),
            None => self.next(ID),
        }
    }
}

/// 没写会话时冲着谁:你在 aca 的某个 tmux 会话里另开的窗口,就是那个会话(看这一格那儿的 `@aca_id`)。
/// 只用来补上没写的 id,不是「我是谁」—— 那是 agent 自己环境里的 `ACA_SESSION_ID`(父会话、消息的发件人看它);
/// agent 自己调的不补(省得它不写 id 就冲着自己发、中止自己)
fn here() -> Option<String> {
    if std::env::var("ACA_SESSION_ID").is_ok_and(|s| !s.is_empty()) || std::env::var_os("TMUX").is_none() {
        return None;
    }
    tmux::session_of(&std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty())?)
}

/// 列表不在终端里打的样子(脚本、agent 读的):一个字不能变,也不随语言变。终端里的归 aca-tui 的 print 排
fn print_rows(rows: &[Row]) {
    i18n::with(Lang::Zh, || print_rows_zh(rows))
}

fn print_rows_zh(rows: &[Row]) {
    let now = paths::now();
    let mut group = None;
    for r in rows {
        if group != Some(&r.group) {
            println!("{}", if r.group.is_empty() { "(没分组)" } else { &r.group });
            group = Some(&r.group);
        }
        let who = if r.account.is_empty() { r.tool.clone() } else { format!("{}:{}", r.tool, agent::account_label(&r.account)) };
        let tokens: String = r.tokens.iter().map(|(k, v)| format!(" [{k} {v}]")).collect();
        println!("  {} {}  {}  {}  {}{}", r.state.glyph(), r.id, r.title, who, print::age(now.saturating_sub(r.active)), tokens);
        if !r.ask.is_empty() {
            println!("      └ {}", r.ask);
        }
    }
}

fn context(store: &Store, key: Option<String>, source: &str) -> Res<Context> {
    // 没指定就是「我现在所在的会话」:agent 自己调的,或者你在它的 tmux 会话里另开的窗口
    let key = key.or_else(|| std::env::var("ACA_SESSION_ID").ok().filter(|s| !s.is_empty())).or_else(here);
    let Some(key) = key else { return Ok(Context { source: source.into(), ..Default::default() }) };
    let s = store.get(&key)?;
    let (tmux, pane) = tmux::find(&s.id).map(|p| (p.sid, p.pane)).unwrap_or_default();
    Ok(Context { session: s.id, dir: s.dir, tmux, pane, parent: s.parent, source: source.into() })
}

/// `json_out`:`--json`(在分到这里之前已经从参数里拿走了)
fn plugin_cmd(store: &Store, mut a: Args, json_out: bool) -> Res<i32> {
    match a.next(SUB)?.as_str() {
        "link" => {
            let (m, warn) = plugin::link(&a.next(Tr::new("插件目录", "a plugin directory"))?)?;
            println!(
                "{}",
                tr!(
                    "链接了 {}({} {}):{} 个动作、{} 个事件钩子、{} 个窗格",
                    "linked {} ({} {}): actions {}, event hooks {}, panes {}",
                    m.id,
                    m.name,
                    m.version,
                    m.actions.len(),
                    m.events.len(),
                    m.panes.len()
                )
            );
            warn.iter().for_each(|w| println!("{}", tr!("  提醒:{w}", "  note: {w}")));
        }
        "install" => {
            let rev = a.opt("--ref")?;
            let what = Tr::new("来源(git 地址、owner/repo 或本地目录)", "a source (a git URL, owner/repo or a local directory)");
            let (m, warn) = plugin::install(&a.next(what)?, rev.as_deref())?;
            println!("{}", tr!("装好了 {}({} {})", "installed {} ({} {})", m.id, m.name, m.version));
            m.brings().iter().for_each(|l| println!("  {l}"));
            warn.iter().for_each(|w| println!("{}", tr!("  提醒:{w}", "  note: {w}")));
        }
        "update" => {
            let ids: Vec<String> = match a.maybe() {
                Some(id) => vec![id],
                None => plugin::registry().into_iter().filter(|e| !e.source.is_empty()).map(|e| e.id).collect(),
            };
            for id in ids {
                match plugin::update(&id) {
                    Ok(said) => println!("{said}"),
                    Err(e) => println!("{id}:{e}"),
                }
            }
        }
        "remove" | "uninstall" => println!("{}", plugin::remove(&a.next(PLUGIN_ID)?)?),
        "unlink" => plugin::unlink(&a.next(PLUGIN_ID)?)?,
        "enable" => plugin::enable(&a.next(PLUGIN_ID)?, true)?,
        "disable" => plugin::enable(&a.next(PLUGIN_ID)?, false)?,
        "ls" => {
            let mut lacks = vec![];
            for e in plugin::registry() {
                let note = match plugin::read_manifest(std::path::Path::new(&e.path)) {
                    Ok((m, _)) => {
                        let missing = m.missing();
                        if !missing.is_empty() {
                            lacks.push(format!("{}:{}", e.id, plugin::missing_note(&missing)));
                        }
                        format!("{} {}", m.name, m.version)
                    }
                    Err(err) => tr!("清单有问题:{err}", "manifest problem: {err}"),
                };
                let from = match e.source.as_str() {
                    "" => tr!("链接 {}", "linked {}", e.path),
                    plugin::first_party::SOURCE => t!("aca 自带", "ships with aca").to_string(),
                    plugin::user::SOURCE => tr!("你自己的 {}", "your own, {}", e.path),
                    src => tr!("装自 {}", "installed from {}", src),
                };
                let agents = e.agents.keys().map(String::as_str).collect::<Vec<_>>();
                let gives =
                    if agents.is_empty() { String::new() } else { tr!("  带给 {}", "  brings to {}", agents.join(t!("、", ", "))) };
                println!("{} {}  {}  {from}{gives}", if e.enabled { "●" } else { "○" }, e.id, note);
            }
            for n in plugin::key_notes().into_iter().map(|(_, n)| n).chain(lacks) {
                println!("⚠ {n}");
            }
            // 钩子、定时上次没跑成,gateway 的服务停下了
            let now = paths::now();
            for p in plugin::health::problems() {
                println!("⚠ {}:{}(aca plugin log {})", p.plugin, p.text(now), p.plugin);
            }
            let stale = plugin::mount::stale();
            if !stale.is_empty() {
                let names: Vec<String> = stale.iter().map(|(id, t)| format!("「{t}」({})", &id[..id.len().min(8)])).collect();
                println!(
                    "{}",
                    tr!(
                        "开着的 {} 起来时带的插件和现在不一样:重启它们才带上(aca restart <id>)",
                        "{} still run with what plugins gave them at start: restart them to pick up the change (aca restart <id>)",
                        names.join(t!("、", ", "))
                    )
                );
            }
        }
        "actions" => {
            for e in plugin::registry().iter().filter(|e| e.enabled) {
                let Ok((m, _)) = plugin::read_manifest(std::path::Path::new(&e.path)) else { continue };
                if json_out {
                    m.actions.iter().for_each(|x| {
                        let line = json!({
                            "id": format!("{}.{}", m.id, x.id), "title": x.title, "contexts": x.contexts, "args": x.args, "expose": x.expose
                        });
                        println!("{line}")
                    });
                } else {
                    m.actions.iter().for_each(|x| {
                        let args: String = x.args.iter().map(|(k, t)| format!(" {k}={t}")).collect();
                        println!("{}.{}{args}  {}  [{}]", m.id, x.id, x.title, x.expose.join(" "))
                    });
                    m.panes.iter().for_each(|p| println!("{}", tr!("{} 窗格 {}  {}", "{} pane {}  {}", m.id, p.id, p.title)));
                    m.views.iter().for_each(|v| println!("{}", tr!("{}.{}  视图  {}", "{}.{}  view  {}", m.id, v.id, v.title)));
                }
            }
        }
        "new" => {
            let id = a.opt("--id")?;
            let dir = a.next(Tr::new("目录", "a directory"))?;
            let id = plugin::dev::scaffold(&dir, id.as_deref())?;
            println!(
                "{}",
                tr!(
                    "建好了 {dir}({id}):aca-plugin.toml 是清单,hello.sh 是它跑的脚本。接着:",
                    "created {dir} ({id}): aca-plugin.toml is the manifest, hello.sh the script it runs. Next:"
                )
            );
            let next = [
                (
                    format!("aca plugin link {dir}"),
                    t!(
                        "链接上(改了脚本马上生效;改了清单再 link 一次)",
                        "link it (script changes apply at once; link again after changing the manifest)"
                    ),
                ),
                (
                    format!("aca plugin run {id}.hello"),
                    t!("跑一下(界面里:会话列表上按 : 再按 h)", "run it (in the interface: : on the session list, then h)"),
                ),
                (format!("aca plugin check {dir}"), t!("查清单", "check the manifest")),
            ];
            let w = next.iter().map(|(c, _)| c.chars().count()).max().unwrap_or(0);
            for (c, what) in next {
                println!("  {c:<w$}  {what}");
            }
            println!("{}", t!("写法:docs/plugins.md(aca help plugin)", "How to write one: docs/plugins.md (aca help plugin)"));
        }
        "check" => {
            let target = a.maybe().unwrap_or_else(|| ".".into());
            let r = plugin::dev::check(&target)?;
            match &r.manifest {
                Some(m) => println!("{} ({} {})  {}", m.id, m.name, m.version, r.dir.display()),
                None => println!("{}", r.dir.display()),
            }
            r.errors.iter().for_each(|e| println!("✕ {e}"));
            r.warns.iter().for_each(|w| println!("⚠ {w}"));
            if r.errors.is_empty() && r.warns.is_empty() {
                println!("{}", t!("✓ 没发现问题", "✓ no problems found"));
            }
            return Ok(i32::from(!r.errors.is_empty()));
        }
        "emit" => {
            let session = a.opt("--session")?;
            let only = a.opt("--plugin")?;
            let event = a.next(Tr::new("事件", "an event"))?;
            let mut fields = serde_json::Map::new();
            while let Some(kv) = a.maybe() {
                let (k, v) = kv.split_once('=').ok_or_else(|| tr!("字段要写成 名字=值:{kv}", "fields are NAME=VALUE: {kv}"))?;
                fields.insert(k.to_string(), plugin::dev::field_value(v));
            }
            let ran = plugin::dev::emit(&event, &context(store, session, "event")?, fields, only.as_deref())?;
            if ran.is_empty() {
                println!(
                    "{}",
                    tr!(
                        "没有开着的插件订了 {event},或者它们的 if 对不上(aca plugin ls 看谁开着)",
                        "no plugin that's on subscribes to {event}, or their if doesn't match (aca plugin ls shows which are on)"
                    )
                );
                return Ok(0);
            }
            for r in &ran {
                let code = r.code.map_or_else(|| t!("被打断", "killed").to_string(), |c| c.to_string());
                println!("{}", tr!("--- {} · {} · 退出码 {code}", "--- {} · {} · exit {code}", r.plugin, r.command));
                print!("{}", r.out);
                if !r.out.is_empty() && !r.out.ends_with('\n') {
                    println!();
                }
            }
            return Ok(i32::from(ran.iter().any(|r| r.code != Some(0))));
        }
        "run" => {
            let session = a.opt("--session")?;
            let what = a.next(Tr::new("插件.动作", "PLUGIN.ACTION"))?;
            // 剩下的是参数:名字=值
            let mut given = vec![];
            while let Some(kv) = a.maybe() {
                let (k, v) = kv.split_once('=').ok_or_else(|| tr!("参数要写成 名字=值:{kv}", "arguments are NAME=VALUE: {kv}"))?;
                given.push((k.to_string(), v.to_string()));
            }
            return plugin::invoke(&what, &context(store, session, "cli")?, &given);
        }
        "timer" | "timers" => {
            let what = Tr::new("插件.能力", "PLUGIN.COMMAND");
            match a.maybe().as_deref() {
                None | Some("ls") => {
                    let rows = plugin::timer::list();
                    if json_out {
                        rows.iter().for_each(|r| println!("{}", serde_json::to_string(r).unwrap_or_default()));
                        return Ok(0);
                    }
                    if rows.is_empty() {
                        println!("{}", t!("没有插件写了定时([[timers]])", "no plugin has timers ([[timers]])"));
                    }
                    let now = paths::now();
                    rows.iter().for_each(|r| println!("{}", r.text(now)));
                    rows.iter().filter_map(plugin::timer::Row::mismatch).for_each(|m| println!("⚠ {m}"));
                }
                Some("on") => println!("{}", plugin::timer::on(&a.next(what)?)?),
                Some("off") => println!("{}", plugin::timer::off(&a.next(what)?)?),
                Some("run") => return plugin::timer::run_now(&a.next(what)?),
                // launchd 到点跑的
                Some("tick") => return Ok(plugin::timer::tick(&a.next(what)?)),
                Some(other) => {
                    SHORT.store(true, Ordering::Relaxed);
                    return Err(tr!("aca plugin timer 没有 {other}", "aca plugin timer has no {other}"));
                }
            }
        }
        sub @ ("allow" | "deny") => {
            let id = a.next(PLUGIN_ID)?;
            let reset = a.flag("--reset");
            let mut names = vec![];
            while let Some(n) = a.maybe() {
                names.push(n);
            }
            if reset {
                plugin::yours::reset_tools(&id)?;
            }
            for n in &names {
                plugin::yours::allow(&id, n, sub == "allow")?;
            }
            let all = plugin::yours::tool_choices(&id)?;
            if all.is_empty() {
                println!("{}", tr!("{id} 没有能给 agent 的能力", "{id} has nothing it can give agents"));
                return Ok(0);
            }
            for (t, on, ask) in &all {
                let note = if *ask && !on { t!("  要你点名才给", "  only if you allow it") } else { "" };
                println!("{} {t}{note}", if *on { "✓" } else { "○" });
            }
            if reset || !names.is_empty() {
                println!(
                    "{}",
                    t!(
                        "开着的会话要重启才变(Codex 只在启动时读工具);gateway 开着时 Claude 当场换",
                        "running sessions change after a restart (Codex reads tools only at start); with the gateway on, Claude switches at once"
                    )
                );
            }
        }
        "key" | "keys" => {
            let Some(id) = a.maybe() else {
                for it in plugin::menu() {
                    let Some(k) = it.key else { continue };
                    println!("{k}  {}.{}  {}", it.plugin, it.id, it.title);
                }
                for (_, n) in plugin::key_notes() {
                    println!("⚠ {n}");
                }
                return Ok(0);
            };
            let target = a.next(Tr::new("能力 / 窗格 / 视图", "a command / pane / view"))?;
            let key = if a.flag("--reset") {
                None
            } else if a.flag("--none") {
                Some(String::new())
            } else {
                Some(a.next(Tr::new("一个键(--none 不要键,--reset 回到清单的)", "a key (--none for no key, --reset for the manifest's)"))?)
            };
            plugin::yours::set_key(&id, &target, key.as_deref())?;
            let now = plugin::menu().into_iter().find(|i| i.plugin == id && i.id == target).and_then(|i| i.key);
            match now {
                Some(k) => println!("{}", tr!("{id}.{target}:: 菜单里按 {k}", "{id}.{target}: press {k} in the : menu")),
                None => println!("{}", tr!("{id}.{target}:没有键(: 菜单里挑)", "{id}.{target}: no key (pick it in the : menu)")),
            }
            for (_, n) in plugin::key_notes().into_iter().filter(|(p, _)| *p == id) {
                println!("⚠ {n}");
            }
        }
        "hotkey" | "hotkeys" => {
            let Some(id) = a.maybe() else {
                // 顺手让 tmux 上的跟现在要的一致;被你自己的绑定占着的在这点名
                let held = plugin::bind_hotkeys();
                let (items, notes) = plugin::hotkeys();
                for h in &items {
                    println!("{}  {}.{}  {}", h.key, h.plugin, h.pane, h.title);
                }
                for (_, n) in notes.into_iter().chain(held) {
                    println!("⚠ {n}");
                }
                return Ok(0);
            };
            let pane = a.next(Tr::new("窗格 id", "a pane id"))?;
            let key = if a.flag("--reset") {
                None
            } else if a.flag("--none") {
                Some(String::new())
            } else {
                Some(a.next(Tr::new(
                    "一个键(tmux 的键名:C-]、M-y;--none 不要,--reset 回到清单的)",
                    "a key (a tmux key name: C-], M-y; --none for none, --reset for the manifest's)",
                ))?)
            };
            plugin::yours::set_hotkey(&id, &pane, key.as_deref())?;
            let held = plugin::bind_hotkeys();
            let (items, notes) = plugin::hotkeys();
            match items.iter().find(|h| h.plugin == id && h.pane == pane) {
                Some(h) => println!("{}", tr!("{id}.{pane}:在会话里按 {}", "{id}.{pane}: press {} inside a session", h.key)),
                None => println!("{}", tr!("{id}.{pane}:没有热键", "{id}.{pane}: no hotkey")),
            }
            for (_, n) in notes.into_iter().chain(held).filter(|(p, _)| *p == id) {
                println!("⚠ {n}");
            }
        }
        "setting" | "settings" => {
            let id = a.next(PLUGIN_ID)?;
            // 先拿掉 --reset:不然它会被当成要写进去的值
            let reset = a.flag("--reset");
            match (a.maybe(), a.maybe()) {
                (None, _) => {
                    // secret 的打码;要原样的值点名读它(给脚本用)
                    for s in plugin::settings_of(&id)? {
                        let changed = if s.changed { t!("(改过)", " (changed)") } else { "" };
                        println!("{} = {}{changed}  {}", s.name, s.shown(), s.desc);
                    }
                }
                (Some(k), None) if reset => plugin::set_setting(&id, &k, None)?,
                (Some(k), None) => {
                    let all = plugin::settings_of(&id)?;
                    let s = all.iter().find(|s| s.name == k).ok_or_else(|| tr!("{id} 没有 {k} 这个设置", "{id} has no setting {k}"))?;
                    println!("{}", plugin::plain(&s.value));
                }
                (Some(k), Some(v)) => plugin::set_setting(&id, &k, Some(&v))?,
            }
        }
        "view" => {
            let session = a.opt("--session")?;
            let raw = json_out;
            let what = a.next(Tr::new("插件.视图", "PLUGIN.VIEW"))?;
            let (ok, out) = plugin::capture(&what, &context(store, session, "cli")?, &[])?;
            // 列表视图打成一行一条(要原样的 JSON 加 --json)
            let rows = plugin::menu().into_iter().any(|i| i.qualified() == what && i.rows);
            match plugin::ViewRows::parse(&out) {
                Ok(l) if ok && rows && !raw => println!("{}", l.text()),
                _ => print!("{out}"),
            }
            return Ok(if ok { 0 } else { 1 });
        }
        "gateway" => match a.maybe().as_deref().unwrap_or("status") {
            "status" => println!("{}", gateway::status_text()),
            "stop" => {
                gateway::ctl(json!({ "cmd": "stop" }))?;
                println!(
                    "{}",
                    t!("停了 gateway(下一个 agent 连上来时会再起)", "stopped the gateway (it starts again when the next agent connects)")
                );
            }
            "restart" => {
                let name = a.maybe().unwrap_or_default();
                let r = gateway::ctl(json!({ "cmd": "restart", "server": name }))?;
                println!(
                    "{}",
                    tr!("重启了 {} 个服务(下次有人要时再起)", "restarted {} servers (they start again when needed)", r["restarted"])
                );
            }
            "log" => {
                let name = a.next(Tr::new("服务名", "a server name"))?;
                print!("{}", std::fs::read_to_string(gateway::log_of(&name)).unwrap_or_default());
            }
            "import" => {
                let apply = a.flag("--apply");
                let mut only = vec![];
                while let Some(n) = a.maybe() {
                    only.push(n);
                }
                let found = gateway::import::scan();
                if found.is_empty() {
                    println!("{}", t!("各账号里没有 MCP", "no MCPs in any account"));
                    return Ok(0);
                }
                for f in &found {
                    // env 只列名字,值不打出来(多半是 key)
                    let mut cmd =
                        std::iter::once(f.command.as_str()).chain(f.args.iter().take(3).map(String::as_str)).collect::<Vec<_>>().join(" ");
                    if f.args.len() > 3 {
                        cmd.push_str(" …");
                    }
                    let env = if f.env.is_empty() {
                        String::new()
                    } else {
                        format!("  env {}", f.env.keys().cloned().collect::<Vec<_>>().join(" "))
                    };
                    let note = match (&f.problem, f.known) {
                        (Some(p), _) => tr!("  ✕ {p}", "  ✕ {p}"),
                        (None, true) => t!("  ([mcps] 里已经有)", "  (already in [mcps])").to_string(),
                        (None, false) => String::new(),
                    };
                    println!("{}  [{}]  {cmd}{env}{note}", f.name, f.from.join(" "));
                }
                if !apply {
                    println!(
                        "{}",
                        t!(
                            "加 --apply 收进 gateway(只写 aca 的 config.toml 和 plugins.toml,账号里的不动);后面可以只写几个名字",
                            "add --apply to serve them through the gateway (writes only aca's config.toml and plugins.toml, not the accounts); you can name just some"
                        )
                    );
                    return Ok(0);
                }
                let done = gateway::import::apply(&found, &only)?;
                println!("{}", tr!("收进了 gateway:{}", "now served through the gateway: {}", done.join(" ")));
                println!(
                    "{}",
                    t!(
                        "账号里的那份还在:gateway 开着时 agent 会两份都看到。确认经 gateway 用着没问题,再去各账号里删掉(删不删你定)",
                        "the accounts still have their copies: with the gateway on, agents see both. Once it works through the gateway, remove them from the accounts (your call)"
                    )
                );
            }
            sub @ ("add" | "drop") => {
                let name = a.next(Tr::new("[mcps] 里的名字", "a name under [mcps]"))?;
                gateway::set_served(&name, sub == "add")?;
                println!(
                    "{}",
                    if sub == "add" {
                        tr!("{name} 经 gateway 给 agent 了(只起一份)", "{name} now reaches agents through the gateway (one copy)")
                    } else {
                        tr!("{name} 不再经 gateway 给", "{name} no longer goes through the gateway")
                    }
                );
            }
            sub @ ("off" | "on") => {
                let session = a.opt("--session")?;
                let name = a.next(Tr::new("服务", "a server"))?;
                let id = context(store, session, "cli")?.session;
                let mut off = gateway::off_for(&id);
                off.retain(|n| *n != name);
                if sub == "off" {
                    off.push(name.clone());
                }
                gateway::set_off(&id, &off)?;
                println!(
                    "{}",
                    if sub == "off" {
                        tr!("这个会话不再从 gateway 看到 {name}", "this session no longer sees {name} through the gateway")
                    } else {
                        tr!("这个会话又能从 gateway 用 {name} 了", "this session can use {name} through the gateway again")
                    }
                );
            }
            other => {
                SHORT.store(true, Ordering::Relaxed);
                return Err(tr!("aca plugin gateway 没有 {other}", "aca plugin gateway has no {other}"));
            }
        },
        "sync" => {
            let said = plugin::sync()?;
            if said.is_empty() {
                println!("{}", t!("跟 plugins.toml 一致,没什么要做的", "matches plugins.toml; nothing to do"));
            }
            said.iter().for_each(|l| println!("{l}"));
        }
        "pane" => {
            let (session, placement, client) = (a.opt("--session")?, a.opt("--placement")?, a.opt("--client")?);
            // 热键按出来的(tmux 上绑的那条命令带着它)。插件已经停了、删了,这个键是上回留下的:解掉,不报错
            let hotkey = a.opt("--hotkey")?;
            let (p, pane) = (a.next(PLUGIN_ID)?, a.next(Tr::new("窗格 id", "a pane id"))?);
            if hotkey.is_some() && !plugin::hotkeys().0.iter().any(|h| h.plugin == p && h.pane == pane) {
                plugin::bind_hotkeys();
                return Ok(0);
            }
            return plugin::open_pane(&p, &pane, placement.as_deref(), client.as_deref(), &context(store, session, "cli")?);
        }
        "log" => print!("{}", std::fs::read_to_string(plugin::log_file(&a.next(PLUGIN_ID)?)).unwrap_or_default()),
        other => {
            SHORT.store(true, Ordering::Relaxed);
            return Err(tr!("aca plugin 没有 {other}", "aca plugin has no {other}"));
        }
    }
    Ok(0)
}

fn events(mut a: Args) -> Res<i32> {
    let follow = a.flag("-f");
    let n: usize = a.opt("-n")?.and_then(|n| n.parse().ok()).unwrap_or(if follow { 0 } else { 20 });
    let path = paths::events_log();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].iter().for_each(|l| println!("{l}"));
    let mut at = text.len() as u64;
    if !follow {
        return Ok(0);
    }
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        // 变小了 = 换了新文件,从头读
        at = at.min(len);
        if len > at {
            let mut f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            f.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
            let mut more = String::new();
            f.read_to_string(&mut more).map_err(|e| e.to_string())?;
            // 只吐完整的行
            let whole = more.rfind('\n').map_or(0, |i| i + 1);
            print!("{}", &more[..whole]);
            at += whole as u64;
        }
    }
}

/// 命令行给的目录:展开 ~、解析成真实路径,要是个已有的目录
fn real_dir(d: &str) -> Res<String> {
    let p = std::path::Path::new(&paths::expand(d)).canonicalize().map_err(|e| format!("{d}:{e}"))?;
    if !p.is_dir() {
        return Err(tr!("{} 不是目录", "{} is not a directory", p.display()));
    }
    Ok(p.to_string_lossy().into_owned())
}

fn group_cmd(store: &Store, mut a: Args) -> Res<i32> {
    match a.next(SUB)?.as_str() {
        "ls" if a.flag("--json") => println!("{}", serde_json::to_string(&store.groups()?).unwrap()),
        "ls" => {
            let groups = store.groups()?;
            if !print::groups(&groups) {
                groups.iter().for_each(|g| println!("{:>3}  {}  {}", g.order, g.path, g.default_path));
            }
        }
        "create" => {
            let path = a.next(GROUP_PATH)?;
            let path = path.trim_matches('/');
            store.group_create(path, "")?;
            // 一个目录只当一个组的默认目录
            if let Some(d) = a.maybe().filter(|d| !d.is_empty()).map(|d| paths::expand(&d)) {
                store.group_set_dir(path, &d)?;
            }
        }
        "rm" => {
            let n = store.group_remove(&a.next(GROUP_PATH)?)?;
            let s = if n == 1 { "" } else { "s" };
            println!("{}", tr!("删了,{n} 个会话挪到了上一级", "deleted; {n} session{s} moved up a level"));
        }
        // 目录登记成分组:名字按目录定(见 Store::dir_group),打出组名
        "add" => println!("{}", store.group_claim(&real_dir(&a.maybe().unwrap_or_else(|| ".".into()))?)?.0),
        "rename" => {
            let from = a.next(GROUP_PATH)?;
            let to = a.next(Tr::new("新的分组路径", "the new group path"))?;
            let n = aca_tui::rename_group(store, &from, &to)?;
            let s = if n == 1 { "" } else { "s" };
            println!(
                "{} → {}{}",
                from.trim_matches('/'),
                to.trim_matches('/'),
                if n > 0 { tr!(",{n} 个会话跟过去", ", {n} session{s} moved along") } else { String::new() }
            );
        }
        "dir" => {
            let path = a.next(GROUP_PATH)?;
            match a.maybe() {
                None => match store.groups()?.into_iter().find(|g| g.path == path) {
                    Some(g) => println!("{}", g.default_path),
                    None => return Err(tr!("没有 {path} 这个分组", "no group {path}")),
                },
                Some(d) if d.is_empty() => store.group_set_dir(&path, "")?,
                Some(d) => store.group_set_dir(&path, &real_dir(&d)?)?,
            }
        }
        "order" => {
            let path = a.next(GROUP_PATH)?;
            store.group_order(&path, a.next(POS)?.parse().map_err(|_| tr!("位置要是个数", "the position must be a number"))?)?;
        }
        other => {
            SHORT.store(true, Ordering::Relaxed);
            return Err(tr!("aca group 没有 {other}", "aca group has no {other}"));
        }
    }
    Ok(0)
}

fn meta_cmd(store: &Store, mut a: Args) -> Res<i32> {
    let sub = a.next(SUB)?;
    let ttl: u64 = a.opt("--ttl")?.map(|t| t.parse().map_err(|_| tr!("--ttl 要是秒数", "--ttl takes seconds"))).transpose()?.unwrap_or(0);
    let s = store.get(&a.next(ID)?)?;
    let name = a.next(Tr::new("名字", "a name"))?;
    if name.is_empty() || name.len() > 32 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(tr!("名字只能是 1–32 个字母、数字、_ -", "a name is 1–32 letters, digits, _ -"));
    }
    match sub.as_str() {
        "set" => {
            // 一行里放得下的一小段:去掉控制字符,截到 80 个字
            let value: String = a.next(Tr::new("值", "a value"))?.chars().filter(|c| !c.is_control()).take(80).collect();
            runtime::update(&s.id, |r| {
                r.tokens.insert(name, Token { value, until: if ttl == 0 { 0 } else { paths::now() + ttl } });
            });
        }
        "rm" => {
            runtime::update(&s.id, |r| {
                r.tokens.remove(&name);
            });
        }
        other => {
            SHORT.store(true, Ordering::Relaxed);
            return Err(tr!("aca meta 没有 {other}", "aca meta has no {other}"));
        }
    }
    Ok(0)
}

/// 「对话 3f2a9c1e」:id 只打开头,够认了
fn short_conv(conv: &str) -> String {
    if conv.is_empty() {
        tr!("对话没认出", "conversation not recognized")
    } else {
        tr!("对话 {}", "conv {}", conv.chars().take(8).collect::<String>())
    }
}

/// 一个能接管的会话认成了什么,一行
fn adopt_line(store: &Store, p: &adopt::Plan) -> String {
    let group = match p.s.group.as_str() {
        "" => String::new(),
        // -g .:真接管时才定,这里写现在会定成哪个
        session::BY_DIR => {
            tr!("  分组 {}(按目录)", "  group {} (by directory)", store.dir_group(&p.s.dir).map(|g| g.0).unwrap_or_else(|e| e))
        }
        g => tr!("  分组 {g}", "  group {g}"),
    };
    let args = if p.s.args.is_empty() { String::new() } else { tr!("  带 {}", "  with {}", p.s.args.join(" ")) };
    tr!("{}  →  「{}」 {}{group}  {}{args}", "{}  →  “{}” {}{group}  {}{args}", p.tmux, p.title(), p.who(), short_conv(&p.s.conv))
}

fn adopt_json(p: &adopt::Plan, tmux: &str) -> serde_json::Value {
    json!({
        "id": p.s.id, "title": p.title(), "tool": p.s.tool, "account": p.s.account, "group": p.s.group, "dir": p.s.dir,
        "conv": p.s.conv, "args": p.s.args, "tmux": tmux, "was": p.tmux, "pane": p.pane, "notes": p.notes,
    })
}

/// `aca adopt`:不带参数列出能接管的;给了会话就接管(`-n` 只看)
fn adopt_cmd(store: &Store, cfg: &config::Config, mut a: Args, args: Option<Vec<String>>, json_out: bool) -> Res<i32> {
    let (all, dry) = (a.flag("--all"), a.flag("-n"));
    let opts = adopt::Opts {
        title: a.opt("-t")?.unwrap_or_default(),
        group: a.opt("-g")?,
        spec: a.opt("--as")?.unwrap_or_default(),
        conv: a.opt("--conv")?.unwrap_or_default(),
        args,
    };
    let targets = std::mem::take(&mut a.0);
    if let Some(odd) = targets.iter().find(|t| t.starts_with('-')) {
        SHORT.store(true, Ordering::Relaxed);
        return Err(tr!("aca adopt 不认 {odd}", "aca adopt doesn't take {odd}"));
    }
    let one_only = !opts.title.is_empty() || !opts.conv.is_empty() || !opts.spec.is_empty() || opts.args.is_some();
    if one_only && (all || targets.len() != 1) {
        return Err(tr!(
            "-t / --as / --conv / -- 参数 是给一个会话的:一次接管一个时才能用",
            "-t / --as / --conv / -- ARGS are for one session: only when adopting exactly one"
        ));
    }
    let plans: Vec<adopt::Plan> = if !targets.is_empty() {
        targets.iter().map(|t| adopt::plan(store, cfg, t, &opts)).collect::<Res<_>>()?
    } else {
        let (mut plans, others) = adopt::candidates(store, cfg)?;
        if let Some(g) = &opts.group {
            plans.iter_mut().for_each(|p| p.s.group = g.trim_matches('/').to_string());
        }
        if !all {
            // 只列出来看
            if json_out {
                println!("{}", serde_json::Value::Array(plans.iter().map(|p| adopt_json(p, &p.tmux)).collect()));
                return Ok(0);
            }
            if plans.is_empty() {
                println!(
                    "{}",
                    tr!(
                        "没有能直接接管的 tmux 会话(要没登记过、第一格里跑着认得出的 claude / codex)",
                        "no tmux session to adopt as is (it must be unregistered, a recognizable claude / codex in the first pane)"
                    )
                );
            } else {
                println!(
                    "{}",
                    tr!(
                        "能接管的(接一个:aca adopt <tmux 会话名或里面的一段>;全接:aca adopt --all):",
                        "can be adopted (one: aca adopt <tmux session name or part of it>; all: aca adopt --all):"
                    )
                );
                plans.iter().for_each(|p| println!("  {}", adopt_line(store, p)));
            }
            if !others.is_empty() {
                let (n, list) = (others.len(), others.join(&tr!("、", ", ")));
                println!(
                    "{}",
                    tr!(
                        "另有 {n} 个 tmux 会话没认出 agent、或者 agent 不在第一格:{list}",
                        "{n} more tmux session(s) with no recognized agent, or the agent not in the first pane: {list}"
                    )
                );
                println!(
                    "{}",
                    tr!(
                        "  要接管得指名:aca adopt <会话 | %窗格> [--as 工具[:账号]]",
                        "  name it to adopt it: aca adopt <SESSION | %PANE> [--as TOOL[:ACCOUNT]]"
                    )
                );
            }
            return Ok(0);
        }
        plans
    };
    let mut out = vec![];
    for p in &plans {
        if dry {
            println!("{}", tr!("会接管  {}", "would adopt  {}", adopt_line(store, p)));
            p.notes.iter().for_each(|n| println!("  · {n}"));
            continue;
        }
        let (s, name) = adopt::apply(store, p)?;
        if json_out {
            // 分组是登记时才定的(-g .):照登记好的写
            out.push(adopt_json(&adopt::Plan { s: s.clone(), ..p.clone() }, &name));
            continue;
        }
        println!("{}", tr!("接管了「{}」 {}  {}", "adopted “{}” {}  {}", p.title(), s.id, p.who()));
        if name != p.tmux {
            println!("{}", tr!("  tmux 会话名:{} → {name}", "  tmux session name: {} → {name}", p.tmux));
        }
        let group = if s.group.is_empty() { tr!("没分组", "no group") } else { tr!("分组 {}", "group {}", s.group) };
        let args =
            if s.args.is_empty() { String::new() } else { tr!(" · 每次启动带 {}", " · every start adds {}", s.args.join(" ")) };
        println!("  {group} · {}{args}", short_conv(&s.conv));
        p.notes.iter().for_each(|n| println!("  · {n}"));
    }
    if json_out && !dry {
        println!("{}", serde_json::Value::Array(out));
    }
    if all && plans.is_empty() {
        println!("{}", tr!("没有能直接接管的 tmux 会话", "no tmux session to adopt as is"));
    }
    Ok(0)
}

/// 一行里放得下的一段:换行、连着的空白收成一个空格,截到 n 个字
fn one_line(s: &str, n: usize) -> String {
    runtime::clip(&s.split_whitespace().collect::<Vec<_>>().join(" "), n)
}

/// `aca wait` 等到一个,打一行
fn wait_line(s: &aca_core::store::Session, o: inbox::Outcome) -> String {
    let r = runtime::load(&s.id);
    match o {
        inbox::Outcome::Done if r.last.trim().is_empty() => format!("{} 空下来了", s.id),
        inbox::Outcome::Done => format!("{} 跑完一轮:「{}」", s.id, one_line(&r.last, 120)),
        inbox::Outcome::Asking => format!("{} 在等确认:「{}」  看选项:aca answer {}", s.id, one_line(&r.ask, 120), s.id),
        inbox::Outcome::Failed if r.error_kind.is_empty() => format!("{} 出错:{}", s.id, one_line(&r.error, 200)),
        inbox::Outcome::Failed => format!("{} 出错({}):{}", s.id, r.error_kind_said(), one_line(&r.error, 200)),
        inbox::Outcome::Exited => format!("{} 退出了", s.id),
        inbox::Outcome::Timeout => format!("{} 还没完(到时间了)", s.id),
    }
}

/// `aca inbox [会话]`:一个会话收到的(最近几条,带着送到没有);不给会话、也不在会话里:哪些会话有没送的
fn inbox_cmd(store: &Store, mut a: Args, json_out: bool) -> Res<i32> {
    let n: usize = a.opt("-n")?.map(|n| n.parse().map_err(|_| "-n 要是条数")).transpose()?.unwrap_or(20);
    let key = a.maybe().or_else(|| std::env::var("ACA_SESSION_ID").ok().filter(|s| !s.is_empty())).or_else(here);
    let Some(key) = key else {
        let mut any = false;
        for s in store.sessions(false)? {
            let waiting = inbox::pending(&s.id).len();
            if waiting > 0 {
                any = true;
                println!("{}  {}  {waiting} 条没送", s.id, status::title(&s, &runtime::load(&s.id)));
            }
        }
        if !any {
            println!("没有没送的消息");
        }
        return Ok(0);
    };
    let s = store.get(&key)?;
    let all = inbox::list(&s.id);
    let tail = &all[all.len().saturating_sub(n)..];
    if json_out {
        let rows: Vec<serde_json::Value> = tail
            .iter()
            .map(|(m, st)| {
                let mut v = serde_json::to_value(m).unwrap();
                v["stage"] = json!(st);
                v
            })
            .collect();
        println!("{}", serde_json::Value::Array(rows));
        return Ok(0);
    }
    if tail.is_empty() {
        println!("{} 的收件箱是空的", s.id);
    }
    let now = paths::now();
    for (m, st) in tail {
        let who = if m.from == "you" { "你".to_string() } else { m.who.clone() };
        let what = if m.kind == "notice" { m.what.clone() } else { "说".into() };
        println!("[{}] {}  {}前  {who} · {what}:{}", st.said(), m.id, print::age(now.saturating_sub(m.ts)), one_line(&m.text, 120));
    }
    Ok(0)
}

/// 界面那边出的错(终端开不了…)也照这里的规矩:说一句,退出码 1
fn ui(args: Vec<String>) -> Res<i32> {
    aca_tui::main(args).map(|()| 0).map_err(|e| e.to_string())
}

/// help 的一页:终端里上色,接到别处照原样
fn page(text: &str) {
    if !print::help(text) {
        println!("{text}");
    }
}

fn unknown(name: &str) -> String {
    // 挪进插件的老命令:指路
    if name == "mounts" {
        return t!(
            "aca mounts 挪进了插件 aca.tailnet:aca plugin run aca.tailnet.list(add port=3000、drop target=<地址>、publish to=on|off);没装的先 aca plugin install aca.tailnet",
            "aca mounts moved into the aca.tailnet plugin: aca plugin run aca.tailnet.list (add port=3000, drop target=<address>, publish to=on|off); install it first with aca plugin install aca.tailnet"
        )
        .into();
    }
    if name == "remote" {
        return t!(
            "aca remote 挪进了插件 aca.remote:aca plugin run aca.remote.run host=<机器> command='<命令>' [note=… within=6h](list、rm id=…、collect、check);\
             server 的地址和口令是它的设置(aca plugin setting aca.remote url …);没装的先 aca plugin install aca.remote",
            "aca remote moved into the aca.remote plugin: aca plugin run aca.remote.run host=<host> command='<command>' [note=… within=6h] (list, rm id=…, collect, check); \
             the server's address and token are its settings (aca plugin setting aca.remote url …); install it first with aca plugin install aca.remote"
        )
        .into();
    }
    match help::guess(name) {
        Some(near) => {
            tr!("没有 {name} 这个命令,是不是 {near}?(aca help 看全部)", "no command {name}; did you mean {near}? (aca help lists them all)")
        }
        None => tr!("没有 {name} 这个命令(aca help 看全部)", "no command {name} (aca help lists them all)"),
    }
}

fn run(mut a: Args) -> Res<i32> {
    // 什么都不带:界面
    let Some(cmd) = a.maybe() else { return ui(vec![]) };
    // hook 最常跑:排在最前面,不开登记表
    match cmd.as_str() {
        "hook" if !help::asked(&a.0) => return Ok(hook::run(&a.next(Tr::new("哪个工具", "which tool"))?)),
        // Claude 起来、每发一句时并排跑的那条:把会话的名字交给它(见 aca_core::hook::name_cmd)
        "hook-name" => return Ok(hook::name_cmd()),
        // hook 自己留的后手:过几秒再看一次标题
        "retitle" => {
            let (id, tool) = (a.next(ID)?, a.next(Tr::new("哪个工具", "which tool"))?);
            return Ok(hook::retitle_later(&id, &tool, a.next(Tr::new("秒数", "seconds"))?.parse().unwrap_or(3)));
        }
        // 起 agent 的那一步(见 aca_core::agent::launch):文件里的开场那句话接在后面那条命令的最后,换成它
        "exec-saying" => {
            let file = a.next(Tr::new("文件", "a file"))?;
            return Err(tr!("起不来:{}", "couldn't start it: {}", agent::exec_saying(&file, &a.0)));
        }
        // 起会话时留的后手(见 aca_core::trust):「信不信任这个目录」出来就替你选信任
        "trust-watch" => {
            let _ = aca_core::trust::watch(&Store::open()?, &config::load(), &a.next(ID)?);
            return Ok(0);
        }
        // zsh 补全每按一次 Tab 问一次(见 complete.rs):不在 help 的表里
        "__complete" => {
            let words = if a.0.first().is_some_and(|w| w == "--") { &a.0[1..] } else { &a.0[..] };
            crate::complete::print(words);
            return Ok(0);
        }
        // 门铃的两条 hook(Claude 的 SessionStart、FileChanged)和送信的后手:跟 hook 一样不开登记表、不说话
        "inbox" if matches!(a.0.first().map(String::as_str), Some("take" | "watch" | "deliver")) => {
            return Ok(match a.next(SUB)?.as_str() {
                "take" => inbox::take_cmd(),
                "watch" => inbox::watch_cmd(),
                _ => {
                    let id = a.next(ID)?;
                    let after = a.opt("--after")?.and_then(|n| n.parse().ok()).unwrap_or(0);
                    let check: Vec<String> = a.opt("--check")?.map(|c| c.split(',').map(String::from).collect()).unwrap_or_default();
                    inbox::deliver_cmd(&id, after, a.flag("--retry"), &check)
                }
            });
        }
        // agent 起的插件 MCP 服务(见 aca_core::plugin::mount):标准输出是协议,不开登记表、不说别的
        "plugin" if matches!(a.0.first().map(String::as_str), Some("mcp" | "tools")) => {
            return match a.next(SUB)?.as_str() {
                "mcp" => plugin::exec_mcp(&a.next(PLUGIN_ID)?),
                _ => plugin::tools::serve(),
            };
        }
        "plugin" if a.0.first().is_some_and(|s| s == "gateway") && matches!(a.0.get(1).map(String::as_str), Some("serve" | "connect")) => {
            return match a.0[1].as_str() {
                "serve" => gateway::serve(),
                _ => gateway::connect::connect(),
            };
        }
        "help" | "-h" | "--help" => {
            return match a.maybe() {
                None => {
                    page(&help::overview());
                    Ok(0)
                }
                Some(what) => match help::of(&what) {
                    Some(text) => {
                        page(&text);
                        Ok(0)
                    }
                    None => Err(unknown(&what)),
                },
            };
        }
        "version" | "--version" | "-V" => {
            println!("aca {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }
        _ => {}
    }
    // 表里没有的名字不认:加命令要在 help.rs 里写上它,不然到不了下面的分支
    if help::find(&cmd).is_none() {
        return Err(unknown(&cmd));
    }
    // `aca <命令> --help`:只说明,不做
    if help::asked(&a.0) {
        page(&help::of(&cmd).unwrap_or_default());
        return Ok(0);
    }
    if cmd == "ui" {
        return ui(a.0);
    }
    if let Some(code) = aca_tui::command(&cmd, &a.0) {
        return Ok(code);
    }
    if cmd == "events" {
        return events(a);
    }
    // 写坏了的配置是当成没写的(hook 不能因为它出错),人敲的命令这里提醒一句
    if let Some(e) = config::problem() {
        eprintln!("{}", tr!("config.toml 写法有问题,当成没写:{e}", "config.toml has a problem and is ignored: {e}"));
    }
    let cfg = config::load();
    // 不建会话,带着账号原样跑工具自己的命令:换成它,终端直接是它的
    if cmd == "run" {
        let spec = a.next(Tr::new("工具[:账号]", "TOOL[:ACCOUNT]"))?;
        let args = if a.0.first().is_some_and(|x| x == "--") { &a.0[1..] } else { &a.0[..] };
        let l = agent::direct(&cfg, &spec, args)?;
        return Err(tr!(
            "起不来:{}",
            "couldn't start it: {}",
            std::process::Command::new(&l.argv[0]).args(&l.argv[1..]).envs(l.env).exec()
        ));
    }
    let store = Store::open()?;
    // `--` 后面的原样交给 agent,不当成 aca 自己的参数
    let passed = a.0.iter().position(|x| x == "--").map(|i| a.0.split_off(i).split_off(1));
    let pass = passed.clone().unwrap_or_default();
    let json_out = a.flag("--json");
    match cmd.as_str() {
        "new" => {
            let say = a.opt("--say")?;
            let notify = a.opt("--notify")?.unwrap_or_default();
            if let Some(odd) =
                notify.split(',').map(str::trim).find(|x| !matches!(*x, "" | "done" | "failed" | "exited" | "waiting" | "all" | "none"))
            {
                return Err(format!("--notify 认 done、failed、exited、waiting(逗号隔开)、all 或者 none,不认 {odd}"));
            }
            let n = New {
                title: a.opt("-t")?.unwrap_or_default(),
                group: a.opt("-g")?.unwrap_or_default(),
                parent: a.opt("--parent")?.unwrap_or_default(),
                hold: a.flag("--hold"),
                notify,
                worktree: a.opt("--worktree")?.or(a.opt("-w")?).unwrap_or_default(),
                tool: a.next(Tr::new("工具(aca names 看有哪些)", "a tool (aca names lists them)"))?,
                dir: a.maybe().unwrap_or_else(|| ".".into()),
                args: pass,
            };
            let wt = !n.worktree.is_empty();
            let (s, unsaid) = match say.filter(|t| !t.trim().is_empty()) {
                Some(text) => session::create_saying(&store, &cfg, n, text.trim_end_matches('\n'))?,
                None => (session::create(&store, &cfg, n)?, None),
            };
            if wt {
                // id 照旧是标准输出的唯一一行(脚本要读),在哪个 worktree 写到标准错误
                eprintln!("{}", aca_core::worktree::note(Path::new(&s.dir)).unwrap_or_else(|| s.dir.clone()));
            }
            // 会话已经建好了:话没发进去不算新建失败,说一声,id 照样打出来
            if let Some(e) = unsaid {
                eprintln!("{}", tr!("建好了,但第一句话没发进去:{e}", "created, but the first message wasn't sent: {e}"));
            }
            if json_out {
                println!("{}", serde_json::to_string(&status::one(&cfg, &s, false)).unwrap());
            } else {
                println!("{}", s.id);
            }
        }
        "ls" => {
            let rows = status::collect(&store, &cfg, !a.flag("--no-screen"))?;
            if json_out {
                println!("{}", serde_json::to_string(&rows).unwrap());
            } else if !print::rows(&cfg, &store, &rows) {
                print_rows(&rows);
            }
        }
        "show" => {
            let s = store.get(&a.id()?)?;
            let (row, r) = (status::one(&cfg, &s, true), runtime::load(&s.id));
            if json_out {
                let mut v = serde_json::to_value(&row).unwrap();
                v["run"] = serde_json::to_value(&r).unwrap();
                v["args"] = json!(s.args);
                println!("{v}");
            } else if !print::show(&cfg, &store, &row, &r) {
                print_rows(std::slice::from_ref(&row));
                println!("  目录 {}\n  对话 {}\n  tmux {} {}", row.dir, row.conv, row.tmux, row.pane);
                if !r.last.is_empty() {
                    println!("  最后一条回复:\n{}", r.last);
                }
            }
        }
        "attach" => {
            let s = store.get(&a.next(ID)?)?;
            let p = tmux::find(&s.id).ok_or_else(|| tr!("会话没在跑(aca start)", "the session isn't running (aca start)"))?;
            runtime::update(&s.id, |r| r.seen = paths::now());
            tmux::bind_detach();
            let verb = if std::env::var_os("TMUX").is_some() { "switch-client" } else { "attach-session" };
            return Err(tr!(
                "tmux 起不来:{}",
                "couldn't run tmux: {}",
                tmux::cmd().args([verb, "-t", &p.sid]).stdin(std::process::Stdio::inherit()).exec()
            ));
        }
        "start" => println!("{}", session::start(&store, &cfg, &a.next(ID)?)?),
        "stop" => session::stop(&store, &a.next(ID)?)?,
        "restart" => {
            let fresh = a.flag("--fresh");
            session::restart(&store, &cfg, &a.next(ID)?, fresh)?;
        }
        "fork" => {
            let title = a.opt("-t")?.unwrap_or_default();
            println!("{}", session::fork(&store, &cfg, &a.next(ID)?, &title)?.id);
        }
        "rename" => {
            let auto = a.flag("--auto");
            let key = a.next(ID)?;
            match auto {
                true => session::unlock(&store, &key)?,
                false => println!("{}", session::rename(&store, &key, &a.next(Tr::new("新名字", "the new name"))?)?),
            }
        }
        "move" => {
            let key = a.next(ID)?;
            let to = a.next(Tr::new("分组", "a group"))?;
            let got = session::move_to(&store, &key, &to)?;
            // 按目录定的:说一声进了哪个
            if to.trim_matches('/') == session::BY_DIR {
                println!("{got}");
            }
        }
        "order" => {
            let s = store.get(&a.next(ID)?)?;
            store.set_order(&s.id, a.next(POS)?.parse().map_err(|_| tr!("位置要是个数", "the position must be a number"))?)?;
        }
        "adopt" => return adopt_cmd(&store, &cfg, a, passed, json_out),
        "release" => {
            let (title, name) = session::release(&store, &a.next(ID)?)?;
            match name.is_empty() {
                true => {
                    println!("{}", tr!("放手了「{title}」:登记删了(它没在跑)", "let go of “{title}”: unregistered (it wasn't running)"))
                }
                false => println!(
                    "{}",
                    tr!(
                        "放手了「{title}」:登记删了,tmux 会话 {name} 和里面的进程照留",
                        "let go of “{title}”: unregistered; tmux session {name} and its processes keep running"
                    )
                ),
            }
        }
        "archive" => session::archive(&store, &a.next(ID)?, true)?,
        "unarchive" => session::archive(&store, &a.next(ID)?, false)?,
        "rm" => {
            let (title, worktree) = session::remove(&store, &a.next(ID)?)?;
            println!("{}", tr!("删了「{title}」", "deleted “{title}”"));
            if let Some(w) = worktree {
                println!("{w}");
            }
        }
        "send" => {
            let wait = a
                .opt("--wait")?
                .map(|w| w.parse::<u64>().map(Duration::from_secs).map_err(|_| tr!("--wait 要是秒数", "--wait takes seconds")))
                .transpose()?;
            let text = match a.opt("-m")? {
                Some(m) => m,
                None => {
                    let mut t = String::new();
                    std::io::stdin().read_to_string(&mut t).map_err(|e| e.to_string())?;
                    t
                }
            };
            if text.trim().is_empty() {
                return Err(tr!("没有要发的话", "nothing to send"));
            }
            session::send(&store, &cfg, &a.id()?, text.trim_end_matches('\n'), wait)?;
        }
        "msg" => {
            let to = a.next(Tr::new("发给谁(会话 id、名字,或者 parent)", "who to (a session id, a name, or parent)"))?;
            let text = if a.0.is_empty() {
                let mut t = String::new();
                std::io::stdin().read_to_string(&mut t).map_err(|e| e.to_string())?;
                t
            } else {
                std::mem::take(&mut a.0).join(" ")
            };
            if text.trim().is_empty() {
                return Err("没有要发的话".into());
            }
            let (m, sent) = inbox::send(&store, &cfg, &to, &text)?;
            println!("放进了 {} 的收件箱({}):{}", m.to, m.id, sent.said());
        }
        "inbox" => return inbox_cmd(&store, a, json_out),
        "wait" => {
            let any = a.flag("--any");
            let timeout =
                a.opt("--timeout")?.map(|w| w.parse::<u64>().map(Duration::from_secs).map_err(|_| "--timeout 要是秒数")).transpose()?;
            let keys = std::mem::take(&mut a.0);
            if keys.is_empty() {
                SHORT.store(true, Ordering::Relaxed);
                return Err("要给会话 id".into());
            }
            let out = inbox::wait(&store, &cfg, &keys, any, timeout)?;
            if out.is_empty() {
                println!("等了 {} 秒,一个都没等到", timeout.map_or(0, |t| t.as_secs()));
                return Ok(inbox::Outcome::Timeout.code());
            }
            out.iter().for_each(|(s, o)| println!("{}", wait_line(s, *o)));
            return Ok(out.iter().map(|(_, o)| o.code()).find(|c| *c != 0).unwrap_or(0));
        }
        "answer" => {
            let key = a.next(ID)?;
            match a.maybe() {
                Some(reply) => println!("{}", answer::answer(&store, &cfg, &key, &reply)?),
                None => {
                    let s = store.get(&key)?;
                    match status::one(&cfg, &s, true).state {
                        status::State::Ask => {
                            let screen = aca_core::tmux::find(&s.id).map(|p| aca_core::tmux::capture(&p.pane)).unwrap_or_default();
                            println!("{}", answer::show(&s, &runtime::load(&s.id), &screen))
                        }
                        _ => println!("{} 现在没在等确认", s.id),
                    }
                }
            }
        }
        "interrupt" => println!("{}", answer::interrupt(&store, &cfg, &a.id()?)?),
        "output" => {
            let last = runtime::load(&store.get(&a.id()?)?.id).last;
            if !print::reply(&last) {
                print!("{last}");
            }
        }
        "seen" => session::seen(&store, &a.next(ID)?)?,
        "group" => return group_cmd(&store, a),
        "meta" => return meta_cmd(&store, a),
        "plugin" => return plugin_cmd(&store, a, json_out),
        "import" => match a.next(Tr::new("从哪搬(agent-deck)", "where from (agent-deck)"))?.as_str() {
            "agent-deck" => import::agent_deck(&store, &cfg, a.flag("--apply"), a.flag("--all"))?,
            other => return Err(tr!("不会从 {other} 搬", "can't import from {other}")),
        },
        "names" => {
            let names = cfg.names();
            if !print::names(&cfg, &names) {
                names.iter().for_each(|n| println!("{n}"));
            }
        }
        "completion" => match a.next(Tr::new("哪种 shell(zsh)", "which shell (zsh)"))?.as_str() {
            "zsh" => print!("{}", crate::complete::zsh()),
            other => return Err(tr!("还不会 {other} 的补全(只有 zsh)", "no completion for {other} yet (only zsh)")),
        },
        // help.rs 的表里有、这里还没写分支
        other => return Err(tr!("{other} 还没接上", "{other} isn't wired up yet")),
    }
    Ok(0)
}

pub fn main(args: Vec<String>) -> i32 {
    let cmd = args.first().cloned().unwrap_or_default();
    match run(Args(args)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            // 参数没给全:把这条命令怎么写接在后面
            if SHORT.load(Ordering::Relaxed)
                && let Some(u) = help::usage(&cmd)
            {
                eprintln!("{}{u}\n{}", help::USAGE, tr!("细节:aca help {cmd}", "details: aca help {cmd}"));
            }
            1
        }
    }
}
