//! Plugin details: the right-hand pane of the plugins page (the lower half on a narrow screen). What can be changed for the
//! highlighted plugin comes in three sections, all written to plugins.toml; last comes what it brings:
//!
//! - **Settings**: one per row, name and current value (changed ones marked); for the highlighted one, the plugins page
//!   wraps the full description under the details (see [`Detail::hint`]).
//!   ⏎ edits (space turns switches on / off), `-` goes back to the default; ones the manifest marks `secret` (webhook URLs,
//!   keys) are masked, and editing starts from empty
//! - **Tools for agents**: ✓ given ○ not given; ones the manifest writes as `agent?` (they touch things on your machine)
//!   are marked "only if allowed". Space toggles
//! - **Plugin menu** (the one `:` opens on the session list): its items in the menu and their keys. ⏎ then a key switches
//!   to that key, ⌫ removes the key, `-` goes back to the manifest's; keys it didn't get (another plugin took them first)
//!   are listed below
//!
//! What only reaches open sessions after a restart (tools for agents) isn't covered here: the bottom of the plugins page
//! names the sessions that need one.
//! ⏎ on the plugins page enters this pane (the highlight moves here), Esc goes back to the list. Changes run in the background
//! (`Job::Plugins`) and are reloaded together with the plugins page when done.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use aca_core::plugin;
use aca_core::{t, tr};

use crate::ops::Job;
use crate::overlay::{Fx, kit};
use crate::plugins::Act;
use crate::theme;

/// The longest input for one value
const INPUT_MAX: usize = 2000;

/// One setting
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Set {
    pub name: String,
    /// The value shown to people (masked if secret)
    pub shown: String,
    /// Pre-filled when editing (empty if secret)
    pub raw: String,
    pub changed: bool,
    pub desc: String,
    /// A switch: Some(whether it's on now)
    pub switch: Option<bool>,
    pub secret: bool,
}

/// One command that can be given to agents
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tool {
    pub id: String,
    pub title: String,
    pub on: bool,
    /// The manifest's `agent?`: given only when you explicitly allow it
    pub ask: bool,
}

/// One item in the `:` menu
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bind {
    pub id: String,
    pub title: String,
    /// The current key
    pub key: Option<char>,
    /// The key in the manifest
    pub default: Option<char>,
    /// What you wrote in plugins.toml (Some("") = no key)
    pub mine: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tune {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
    pub settings: Vec<Set>,
    pub tools: Vec<Tool>,
    pub binds: Vec<Bind>,
    /// What it brings agents: (label, value)
    pub info: Vec<(String, String)>,
    /// Keys this plugin didn't get, things written wrong
    pub notes: Vec<String>,
    /// Subscribed to events (has a log to look at)
    pub log: bool,
    /// Your own plugin: its manifest (e opens that, not plugins.toml)
    pub mine: Option<String>,
}

/// Read one plugin: the manifest, what you set in plugins.toml, what it brings
pub fn load(id: &str) -> Tune {
    let Some(e) = plugin::registry().into_iter().find(|e| e.id == id) else { return Tune { id: id.into(), ..Default::default() } };
    let mine = (e.source == plugin::user::SOURCE).then(|| format!("{}/{}", e.path, plugin::MANIFEST));
    let Ok((m, _)) = plugin::read_manifest(std::path::Path::new(&e.path)) else {
        return Tune { id: id.into(), enabled: e.enabled, mine, ..Default::default() };
    };
    let title = |id: &str, t: &str| if t.is_empty() { id.to_string() } else { t.to_string() };
    let settings = plugin::settings_of(id)
        .unwrap_or_default()
        .into_iter()
        .map(|s| Set {
            shown: s.shown(),
            raw: if s.secret { String::new() } else { plugin::plain(&s.value) },
            switch: s.value.as_bool(),
            name: s.name,
            changed: s.changed,
            desc: s.desc,
            secret: s.secret,
        })
        .collect();
    let tools = plugin::yours::tool_choices(id)
        .unwrap_or_default()
        .into_iter()
        .map(|(tid, on, ask)| {
            let t = m.actions.iter().find(|a| a.id == tid).map(|a| title(&a.id, &a.title)).unwrap_or_else(|| tid.clone());
            Tool { id: tid, title: t, on, ask }
        })
        .collect();
    let menu: Vec<plugin::MenuItem> = plugin::menu().into_iter().filter(|i| i.plugin == id).collect();
    let want = plugin::decl::want(id);
    let default = |x: &str| m.keys.iter().find(|k| k.run == x).and_then(|k| k.key.chars().next());
    let items = m
        .actions
        .iter()
        .filter(|a| a.exposed("menu"))
        .map(|a| (&a.id, &a.title))
        .chain(m.panes.iter().map(|p| (&p.id, &p.title)))
        .chain(m.views.iter().map(|v| (&v.id, &v.title)));
    let binds = items
        .map(|(bid, t)| Bind {
            id: bid.clone(),
            title: title(bid, t),
            key: menu.iter().find(|i| &i.id == bid).and_then(|i| i.key),
            default: default(bid),
            mine: want.keys.get(bid).cloned(),
        })
        .collect();
    let mut info = vec![];
    let mut gives = vec![];
    let dirs: Vec<&str> = e.agents.keys().map(String::as_str).collect();
    if !dirs.is_empty() {
        gives.push(tr!("{} 的 skill / 斜杠命令", "skills / slash commands for {}", dirs.join(t!("、", ", "))));
    }
    if e.mcp.is_some() {
        gives.push(t!("一个 MCP 服务", "an MCP server").to_string());
    }
    if !gives.is_empty() {
        let to = if want.agents.is_empty() { String::new() } else { tr!(",仅限 {}", "; only for {}", want.agents.join(" ")) };
        info.push((t!("提供给 agent", "agents").to_string(), format!("{}{to}", gives.join(" · "))));
    }
    let mut notes: Vec<String> = plugin::key_notes().into_iter().filter(|(p, _)| p == id).map(|(_, n)| n).collect();
    let missing = m.missing();
    if !missing.is_empty() {
        notes.push(plugin::missing_note(&missing));
    }
    Tune {
        id: id.into(),
        name: m.name,
        version: m.version,
        description: m.description,
        enabled: e.enabled,
        settings,
        tools,
        binds,
        info,
        notes,
        log: !m.events.is_empty() || plugin::log_file(id).exists(),
        mine,
    }
}

/// Which item of which section the highlight is on
#[derive(Clone, Copy, Debug, PartialEq)]
enum At {
    Set(usize),
    Tool(usize),
    Bind(usize),
}

/// After a key in this pane: stay here, or go back to the list
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Back {
    Stay,
    Leave,
}

/// One plugin's details: the right-hand pane of the plugins page
pub struct Detail {
    pub data: Tune,
    sel: usize,
    /// Editing a setting's value
    input: Option<String>,
    /// ⏎ on a plugin menu item: waiting for you to press its new key
    waiting: bool,
}

/// A section heading: bold, followed by a dim note, then a thin rule filling the rest of the line
fn heading(text: &str, note: &str, w: usize) -> Line<'static> {
    let mut spans = vec![Span::styled(text.to_string(), Style::new().add_modifier(Modifier::BOLD))];
    let mut used = text.width();
    let room = w.saturating_sub(used + 4);
    if !note.is_empty() && room > 8 {
        let n = kit::clip(note, room).trim_end().to_string();
        used += 2 + n.width();
        spans.push(Span::styled(format!("  {n}"), kit::dim()));
    }
    if w > used + 2 {
        spans.push(Span::styled(format!(" {}", "─".repeat(w - used - 1)), theme::faint()));
    }
    Line::from(spans)
}

/// A selectable row: when highlighted, an accent-colored bar at its start and the selected background
fn pick(mut spans: Vec<Span<'static>>, w: usize, on: bool) -> Line<'static> {
    if on {
        spans[0] = Span::styled("▌ ", theme::accent());
    }
    kit::picked(Line::from(spans), w, on)
}

impl Detail {
    pub fn new(data: Tune) -> Detail {
        Detail { data, sel: 0, input: None, waiting: false }
    }

    /// The same plugin was reloaded: swap in the new data, the highlight stays on the same row
    pub fn swap(&mut self, data: Tune) {
        self.data = data;
        self.sel = self.sel.min(self.rows().saturating_sub(1));
    }

    /// How many selectable rows (settings + tools + menu items)
    pub fn rows(&self) -> usize {
        self.data.settings.len() + self.data.tools.len() + self.data.binds.len()
    }

    fn at(&self) -> Option<At> {
        let (s, t) = (self.data.settings.len(), self.data.tools.len());
        match self.sel {
            i if i < s => Some(At::Set(i)),
            i if i < s + t => Some(At::Tool(i - s)),
            i if i < self.rows() => Some(At::Bind(i - s - t)),
            _ => None,
        }
    }

    /// Typing / waiting for you to press a key
    pub fn typing(&self) -> bool {
        self.waiting || self.input.is_some()
    }

    /// How a setting is shown: a switch says on / off, an empty one (an empty list too) says "empty"
    fn value(s: &Set) -> String {
        match s.switch {
            Some(true) => t!("开", "on").to_string(),
            Some(false) => t!("关", "off").to_string(),
            None if Self::empty(s) => t!("未填写", "empty").to_string(),
            None => s.shown.clone(),
        }
    }

    fn empty(s: &Set) -> bool {
        s.switch.is_none() && (s.shown.is_empty() || s.shown == "[]")
    }

    /// The highlighted setting: (name, description). The plugins page wraps the full description under the details (not in
    /// the row)
    pub fn hint(&self) -> Option<(&str, &str)> {
        match self.at() {
            Some(At::Set(i)) => Some(&self.data.settings[i]).filter(|s| !s.desc.is_empty()).map(|s| (s.name.as_str(), s.desc.as_str())),
            _ => None,
        }
    }

    /// The settings section: name and value aligned in two columns, changed ones marked (the description isn't in the row,
    /// see [`Detail::hint`])
    fn set_lines(&self, w: usize, focused: bool, out: &mut Vec<Line<'static>>, hl: &mut Option<usize>) {
        let name_w = self.data.settings.iter().map(|s| s.name.width()).max().unwrap_or(0).min(16);
        let val_w = self.data.settings.iter().map(|s| Self::value(s).width()).max().unwrap_or(0).clamp(4, 22);
        for (i, s) in self.data.settings.iter().enumerate() {
            let on = focused && self.at() == Some(At::Set(i));
            if on {
                *hl = Some(out.len());
            }
            let name = format!("{:<name_w$}", kit::clip(&s.name, name_w).trim_end());
            let mut spans = vec![Span::raw("  "), Span::raw(name), Span::raw("  ")];
            match (&self.input, on) {
                (Some(text), true) => {
                    let note = if s.secret { t!("不显示原值", "old value hidden") } else { "" };
                    let room = w.saturating_sub(name_w + 8 + if note.is_empty() { 0 } else { note.width() + 2 });
                    let shown: String = text
                        .chars()
                        .rev()
                        .scan(0, |n, c| {
                            *n += c.to_string().width();
                            (*n <= room).then_some(c)
                        })
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    spans.extend([Span::styled("> ", theme::accent().add_modifier(Modifier::BOLD)), Span::raw(shown), Span::raw("▏")]);
                    let line = kit::spread(spans, vec![Span::styled(note.to_string(), kit::dim())], w);
                    out.push(pick(line.spans, w, on));
                    continue;
                }
                _ => {
                    let v = kit::clip(&Self::value(s), w.saturating_sub(name_w + 12)).trim_end().to_string();
                    let st = match (Self::empty(s), s.changed) {
                        (true, _) => kit::dim(),
                        (false, true) => Style::new().add_modifier(Modifier::BOLD),
                        (false, false) => Style::new(),
                    };
                    // The "changed" column lines up after the longest value
                    let pad = val_w.saturating_sub(v.width());
                    spans.push(Span::styled(v, st));
                    if s.changed {
                        spans.push(Span::raw(" ".repeat(pad + 2)));
                        spans.push(Span::styled(t!("已修改", "changed"), theme::accent()));
                    }
                }
            }
            out.push(pick(spans, w, on));
        }
    }

    fn tool_line(t: &Tool, w: usize, on: bool) -> Line<'static> {
        let (mark, st) = if t.on { ("✓", theme::accent().add_modifier(Modifier::BOLD)) } else { ("○", kit::dim()) };
        let right = if t.ask && !t.on { t!("需明确允许", "only if allowed") } else { "" };
        let room = w.saturating_sub(right.width() + 8);
        let title = kit::clip(&t.title, room).trim_end().to_string();
        let line = kit::spread(
            vec![Span::raw("  "), Span::styled(mark, st), Span::raw(" "), Span::raw(title)],
            vec![Span::styled(right.to_string(), theme::warn())],
            w,
        );
        pick(line.spans, w, on)
    }

    fn bind_line(&self, b: &Bind, w: usize, on: bool) -> Line<'static> {
        if on && self.waiting {
            let text = tr!("为「{}」按下一个键", "press a key for “{}”", b.title);
            let text = kit::clip(&text, w.saturating_sub(7)).trim_end().to_string();
            return pick(
                vec![Span::raw("  "), Span::styled("?", kit::key_style()), Span::raw("  "), Span::styled(text, theme::accent())],
                w,
                on,
            );
        }
        let key = match b.key {
            Some(k) => Span::styled(k.to_string(), kit::key_style().fg(theme::pal().accent)),
            None => Span::styled("–", theme::faint()),
        };
        let right = match (&b.mine, b.default) {
            (Some(m), _) if m.is_empty() => t!("已移除按键", "key removed by you").to_string(),
            (Some(_), Some(d)) => tr!("已修改(原为 {d})", "yours (was {d})"),
            (Some(_), None) => t!("自定义", "added by you").to_string(),
            (None, Some(d)) if b.key != Some(d) => tr!("{d} 已被其他插件占用", "{d} taken by another plugin"),
            (None, _) => String::new(),
        };
        let room = w.saturating_sub(right.width() + 8);
        let text = kit::clip(&b.title, room).trim_end().to_string();
        let line = kit::spread(vec![Span::raw("  "), key, Span::raw("  "), Span::raw(text)], vec![Span::styled(right, kit::dim())], w);
        pick(line.spans, w, on)
    }

    /// This pane's lines; focused = the highlight is in this pane. Also gives the index of the highlighted line (with many
    /// lines, scrolling follows it)
    pub fn body(&self, w: usize, focused: bool) -> (Vec<Line<'static>>, Option<usize>) {
        let d = &self.data;
        let mut out = vec![];
        let mut hl = None;
        if !d.settings.is_empty() {
            out.push(heading(t!("设置", "Settings"), "", w));
            self.set_lines(w, focused, &mut out, &mut hl);
            out.push(Line::raw(""));
        }
        if !d.tools.is_empty() {
            out.push(heading(t!("提供给 agent 的工具", "Tools for agents"), "", w));
            let s = d.settings.len();
            for (i, t) in d.tools.iter().enumerate() {
                let on = focused && self.sel == s + i;
                if on {
                    hl = Some(out.len());
                }
                out.push(Self::tool_line(t, w, on));
            }
            out.push(Line::raw(""));
        }
        if !d.binds.is_empty() {
            let note = if d.enabled { "" } else { t!("关闭时不在菜单中显示", "not in the menu while off") };
            out.push(heading(t!(": 菜单中的按键", "Keys in the : menu"), note, w));
            let s = d.settings.len() + d.tools.len();
            for (i, b) in d.binds.iter().enumerate() {
                let on = focused && self.sel == s + i;
                if on {
                    hl = Some(out.len());
                }
                out.push(self.bind_line(b, w, on));
            }
            out.push(Line::raw(""));
        }
        for n in &d.notes {
            out.extend(crate::ui::wrap_width(&format!("⚠ {n}"), w).into_iter().map(|l| Line::styled(l, theme::warn())));
        }
        let label_w = d.info.iter().map(|(l, _)| l.width()).max().unwrap_or(0) + 2;
        for (l, v) in &d.info {
            let pad = label_w.saturating_sub(l.width());
            let v = kit::clip(v, w.saturating_sub(label_w)).trim_end().to_string();
            out.push(Line::from(vec![Span::styled(format!("{l}{}", " ".repeat(pad)), kit::dim()), Span::raw(v)]));
        }
        (out, hl)
    }

    /// The keys shown at the bottom while the highlight is in this pane
    pub fn keys(&self) -> Vec<(&'static str, &'static str)> {
        if self.waiting {
            return vec![("Esc", t!("取消", "keep"))];
        }
        if self.input.is_some() {
            return vec![("⏎", t!("保存", "save")), ("^U", t!("清空", "clear")), ("Esc", t!("取消", "cancel"))];
        }
        match self.at() {
            Some(At::Set(i)) if self.data.settings[i].switch.is_some() => {
                vec![(t!("空格", "space"), t!("开/关", "on/off")), ("-", t!("默认", "default"))]
            }
            Some(At::Set(_)) => vec![("⏎", t!("修改", "edit")), ("-", t!("默认", "default"))],
            Some(At::Tool(_)) => vec![(t!("空格", "space"), t!("提供/不提供", "give/withhold"))],
            Some(At::Bind(_)) => {
                vec![("⏎", t!("更换按键", "set key")), ("⌫", t!("移除按键", "no key")), ("-", t!("恢复原值", "manifest's"))]
            }
            None => vec![],
        }
    }

    fn job(&self, a: Act, fx: &mut Fx) {
        fx.job = Some(Job::Plugins(a));
    }

    pub fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Back {
        let n = self.rows();
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let id = self.data.id.clone();
        if self.waiting {
            self.waiting = false;
            // Any other key (Esc) = no change, nothing to say
            if let (KeyCode::Char(c), Some(At::Bind(i))) = (k.code, self.at())
                && !ctrl
            {
                let b = self.data.binds[i].id.clone();
                self.job(Act::Key(id, b, Some(c.to_string())), fx);
            }
            return Back::Stay;
        }
        if let Some(text) = &mut self.input {
            match k.code {
                KeyCode::Esc => self.input = None,
                KeyCode::Enter => {
                    let v = std::mem::take(text);
                    self.input = None;
                    if let Some(At::Set(i)) = self.at() {
                        let s = &self.data.settings[i];
                        // A secret starts empty: entering nothing means no change
                        if !(s.secret && v.is_empty()) {
                            self.job(Act::Setting(id, s.name.clone(), Some(v)), fx);
                        }
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char('u') if ctrl => text.clear(),
                KeyCode::Char(c) if !ctrl && text.chars().count() < INPUT_MAX => text.push(c),
                _ => {}
            }
            return Back::Stay;
        }
        match (k.code, self.at()) {
            (KeyCode::Esc | KeyCode::Left | KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('q' | 'h'), _) => return Back::Leave,
            (KeyCode::Down | KeyCode::Char('j'), _) if n > 0 => self.sel = (self.sel + 1) % n,
            (KeyCode::Up | KeyCode::Char('k'), _) if n > 0 => self.sel = (self.sel + n - 1) % n,
            (KeyCode::Enter | KeyCode::Char(' '), Some(At::Set(i))) if self.data.settings[i].switch.is_some() => {
                let s = &self.data.settings[i];
                let to = !s.switch.unwrap_or(false);
                self.job(Act::Setting(id, s.name.clone(), Some(to.to_string())), fx);
            }
            (KeyCode::Enter, Some(At::Set(i))) => self.input = Some(self.data.settings[i].raw.clone()),
            (KeyCode::Char('-'), Some(At::Set(i))) => {
                let s = &self.data.settings[i];
                if s.changed {
                    self.job(Act::Setting(id, s.name.clone(), None), fx);
                } else {
                    fx.say(tr!("{} 已是默认值", "{} is already the default", s.name));
                }
            }
            (KeyCode::Char(' ') | KeyCode::Enter, Some(At::Tool(i))) => {
                let t = &self.data.tools[i];
                self.job(Act::Tool(id, t.id.clone(), !t.on), fx);
            }
            (KeyCode::Enter, Some(At::Bind(_))) => self.waiting = true,
            (KeyCode::Backspace | KeyCode::Delete, Some(At::Bind(i))) => {
                self.job(Act::Key(id, self.data.binds[i].id.clone(), Some(String::new())), fx)
            }
            (KeyCode::Char('-'), Some(At::Bind(i))) => self.job(Act::Key(id, self.data.binds[i].id.clone(), None), fx),
            _ => {}
        }
        Back::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aca_core::i18n::{self, Lang};

    fn fake(id: &str) -> Tune {
        Tune {
            id: id.into(),
            name: "Discord".into(),
            version: "0.1.0".into(),
            description: "跑完了、在等你确认、出错了,往 Discord 频道发一条".into(),
            enabled: true,
            settings: vec![
                Set {
                    name: "url".into(),
                    shown: "••••abcd".into(),
                    changed: true,
                    desc: "webhook 地址".into(),
                    secret: true,
                    ..Default::default()
                },
                Set {
                    name: "workers".into(),
                    shown: "false".into(),
                    raw: "false".into(),
                    desc: "worker 也发".into(),
                    switch: Some(false),
                    ..Default::default()
                },
                Set { name: "username".into(), shown: "aca".into(), raw: "aca".into(), desc: "署名".into(), ..Default::default() },
            ],
            tools: vec![
                Tool { id: "list".into(), title: "列出挂载".into(), on: true, ask: false },
                Tool { id: "add".into(), title: "挂一个端口".into(), on: false, ask: true },
            ],
            binds: vec![
                Bind { id: "mounts".into(), title: "挂载".into(), key: Some('m'), default: Some('m'), mine: None },
                Bind { id: "tasks".into(), title: "任务".into(), key: Some('T'), default: Some('t'), mine: Some("T".into()) },
                Bind { id: "mail".into(), title: "邮件".into(), key: None, default: Some('e'), mine: None },
            ],
            info: vec![("给 agent".into(), "claude 的 skill / 斜杠命令".into())],
            notes: vec!["键 e 已分配给 x「邮件」,aca.discord「邮件」未分配到".into()],
            log: true,
            mine: None,
        }
    }

    fn text(d: &Detail, w: usize, focused: bool) -> String {
        d.body(w, focused)
            .0
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn press(d: &mut Detail, code: KeyCode) -> Fx {
        let mut fx = Fx::default();
        d.on_key(KeyEvent::from(code), &mut fx);
        fx
    }

    fn job(fx: Fx) -> Option<Act> {
        match fx.job {
            Some(Job::Plugins(a)) => Some(a),
            _ => None,
        }
    }

    #[test]
    fn settings_tools_and_menu_keys_each_line_says_its_value_and_who_changed_it() {
        i18n::with(Lang::Zh, || {
            let d = Detail::new(fake("aca.discord"));
            let t = text(&d, 84, true);
            // Settings: name and value aligned, changed ones marked, switches say on / off, secrets masked; the highlighted
            // row starts with a bar. Descriptions aren't in the row
            assert!(t.contains("▌ url       ••••abcd  已修改\n"), "{t}");
            assert!(t.contains("  workers   关\n") && !t.contains("worker 也发"), "{t}");
            // The highlighted setting: name and description go to the plugins page to write below
            assert_eq!(d.hint(), Some(("url", "webhook 地址")));
            // Tools for agents: title only, no id; whether a restart is needed isn't covered here
            assert!(t.contains("提供给 agent 的工具 ─") && t.contains("✓ 列出挂载\n") && t.contains("需明确允许"), "{t}");
            // Plugin menu: the heading says these are keys in the : menu
            assert!(t.contains(": 菜单中的按键 ─"), "{t}");
            assert!(t.contains("m  挂载") && t.contains("已修改(原为 t)") && t.contains("e 已被其他插件占用"), "{t}");
            assert!(t.contains("⚠ 键 e 已分配给") && t.contains("给 agent  claude 的 skill / 斜杠命令"), "{t}");
            assert_eq!(d.keys()[0], ("⏎", "修改"));
            // Highlight not in this pane: no row marked
            let (_, hl) = d.body(84, false);
            assert!(hl.is_none() && !text(&d, 84, false).contains('▌'));
            // A narrow screen (phone) doesn't overflow
            assert!(d.body(40, true).0.iter().all(|l| l.width() <= 40), "{}", text(&d, 40, true));
            // Highlight on tools / menu keys: no setting description
            let mut d = d;
            for _ in 0..3 {
                press(&mut d, KeyCode::Char('j'));
            }
            assert_eq!(d.hint(), None);
        });
    }

    #[test]
    fn enter_edits_a_setting_space_flips_a_switch_and_minus_goes_back_to_the_default() {
        i18n::with(Lang::Zh, || {
            let mut d = Detail::new(fake("aca.discord"));
            // A secret: starts empty and says the old value is hidden; entering nothing = no change
            press(&mut d, KeyCode::Enter);
            assert!(d.typing() && text(&d, 84, true).contains("> ▏") && text(&d, 84, true).contains("不显示原值"));
            assert!(job(press(&mut d, KeyCode::Enter)).is_none());
            press(&mut d, KeyCode::Enter);
            for c in "https://x/y".chars() {
                press(&mut d, KeyCode::Char(c));
            }
            assert_eq!(
                job(press(&mut d, KeyCode::Enter)),
                Some(Act::Setting("aca.discord".into(), "url".into(), Some("https://x/y".into())))
            );
            assert_eq!(job(press(&mut d, KeyCode::Char('-'))), Some(Act::Setting("aca.discord".into(), "url".into(), None)));
            // A switch: space toggles; - on an unchanged one says it's already the default
            press(&mut d, KeyCode::Char('j'));
            assert_eq!(
                job(press(&mut d, KeyCode::Char(' '))),
                Some(Act::Setting("aca.discord".into(), "workers".into(), Some("true".into())))
            );
            let fx = press(&mut d, KeyCode::Char('-'));
            assert!(fx.job.is_none() && fx.flash.unwrap().contains("已是默认值"));
            // A plain one: pre-filled with the current value, saved after editing; while typing, q is a character, not back
            // to the list
            press(&mut d, KeyCode::Char('j'));
            press(&mut d, KeyCode::Enter);
            assert!(text(&d, 84, true).contains("> aca▏"));
            assert_eq!(d.on_key(KeyEvent::from(KeyCode::Char('q')), &mut Fx::default()), Back::Stay);
            assert_eq!(
                job(press(&mut d, KeyCode::Enter)),
                Some(Act::Setting("aca.discord".into(), "username".into(), Some("acaq".into())))
            );
            press(&mut d, KeyCode::Enter);
            assert!(press(&mut d, KeyCode::Esc).job.is_none() && !d.typing());
        });
    }

    #[test]
    fn space_gives_a_tool_and_menu_keys_are_set_removed_or_reset() {
        i18n::with(Lang::Zh, || {
            let mut d = Detail::new(fake("aca.tailnet"));
            for _ in 0..3 {
                press(&mut d, KeyCode::Char('j'));
            }
            assert_eq!(job(press(&mut d, KeyCode::Char(' '))), Some(Act::Tool("aca.tailnet".into(), "list".into(), false)));
            press(&mut d, KeyCode::Char('j'));
            assert_eq!(job(press(&mut d, KeyCode::Char(' '))), Some(Act::Tool("aca.tailnet".into(), "add".into(), true)));
            press(&mut d, KeyCode::Char('j'));
            press(&mut d, KeyCode::Enter);
            assert!(d.typing() && text(&d, 84, true).contains("为「挂载」按下一个键") && d.keys() == [("Esc", "取消")]);
            // Esc: no change, nothing to say
            let fx = press(&mut d, KeyCode::Esc);
            assert!(fx.job.is_none() && fx.flash.is_none() && !d.typing());
            press(&mut d, KeyCode::Enter);
            assert_eq!(job(press(&mut d, KeyCode::Char('q'))), Some(Act::Key("aca.tailnet".into(), "mounts".into(), Some("q".into()))));
            assert_eq!(job(press(&mut d, KeyCode::Backspace)), Some(Act::Key("aca.tailnet".into(), "mounts".into(), Some(String::new()))));
            assert_eq!(job(press(&mut d, KeyCode::Char('-'))), Some(Act::Key("aca.tailnet".into(), "mounts".into(), None)));
            // Esc goes back to the list; reloading the same plugin keeps the highlight on the same row
            d.swap(fake("aca.tailnet"));
            assert_eq!(d.keys()[0], ("⏎", "更换按键"));
            assert_eq!(d.on_key(KeyEvent::from(KeyCode::Esc), &mut Fx::default()), Back::Leave);
        });
    }
}
