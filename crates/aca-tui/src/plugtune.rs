//! 插件详情:插件页右边那一栏(窄屏是下半截)。高亮的插件能改的分三段,都写进 plugins.toml;最后是它带了什么:
//!
//! - **设置**:一行一个,名字、现在的值(改过的标出来)、说明。⏎ 改(开关类空格切换),`-` 回到默认;
//!   清单里标了 `secret` 的(webhook 地址、key)打码显示,改的时候从空的开始填
//! - **给 agent 的工具**:✓ 给了 ○ 没给;清单写 `agent?` 的(会动你机器上东西的)标「要你点名」。空格切换
//! - **插件菜单**(会话列表上按 `:` 出来的那个):它在菜单里的几项和键。⏎ 再按一个键换成它,⌫ 不要键,
//!   `-` 回到清单的;没拿到的(别的插件先拿了)写在下面
//!
//! 插件页上 ⏎ 进到这一栏(高亮落到这里),Esc 回列表。改动放到后台跑(`Job::Plugins`),做完和插件页一起重读。

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

/// 一个值最多输这么长
const INPUT_MAX: usize = 2000;

/// 一个设置
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Set {
    pub name: String,
    /// 给人看的值(secret 的打码了)
    pub shown: String,
    /// 改的时候先填上的(secret 的是空的)
    pub raw: String,
    pub changed: bool,
    pub desc: String,
    /// 开关:Some(现在开没开)
    pub switch: Option<bool>,
    pub secret: bool,
}

/// 一个能给 agent 的能力
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tool {
    pub id: String,
    pub title: String,
    pub on: bool,
    /// 清单写的 `agent?`:要你点名才给
    pub ask: bool,
}

/// `:` 菜单里的一个
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bind {
    pub id: String,
    pub title: String,
    /// 现在的键
    pub key: Option<char>,
    /// 清单写的键
    pub default: Option<char>,
    /// plugins.toml 里你写的(Some("") = 不要键)
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
    /// 它带了什么、从哪来:(标签, 值)
    pub info: Vec<(String, String)>,
    /// 这个插件没拿到的键、写得不对的
    pub notes: Vec<String>,
    /// 订了事件(有日志可看)
    pub log: bool,
    /// 你自己的那份:它的清单(e 打开它,不打开 plugins.toml)
    pub mine: Option<String>,
}

/// 读一个插件:清单、你在 plugins.toml 里定的、它带了什么
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
        let to = if want.agents.is_empty() { String::new() } else { tr!(",只给 {}", "; only for {}", want.agents.join(" ")) };
        info.push((t!("给 agent", "agents").to_string(), format!("{}{to}", gives.join(" · "))));
    }
    let mut on: Vec<&str> = m.events.iter().map(|h| h.on.as_str()).collect();
    on.dedup();
    if !on.is_empty() {
        info.push((t!("事件", "events").to_string(), on.join(" ")));
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

/// 高亮停在哪一段的第几个
#[derive(Clone, Copy, Debug, PartialEq)]
enum At {
    Set(usize),
    Tool(usize),
    Bind(usize),
}

/// 这一栏按完一个键:留在这里,还是回列表
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Back {
    Stay,
    Leave,
}

/// 一个插件的详情:插件页右边那一栏
pub struct Detail {
    pub data: Tune,
    sel: usize,
    /// 正在改一个设置的值
    input: Option<String>,
    /// ⏎ 了插件菜单里的一项:等你按它的新键
    waiting: bool,
}

/// 一段的标题:粗体,后面跟一句淡的说明,再拿细线补满一行
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

/// 能挑的一行:高亮时行首一道强调色的竖条、铺上选中的底色
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

    /// 同一个插件重读了:换上新的,高亮留在原来那行
    pub fn swap(&mut self, data: Tune) {
        self.data = data;
        self.sel = self.sel.min(self.rows().saturating_sub(1));
    }

    /// 能挑的有几行(设置 + 工具 + 菜单项)
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

    /// 正在打字 / 等你按一个键
    pub fn typing(&self) -> bool {
        self.waiting || self.input.is_some()
    }

    /// 一个设置显示成什么:开关写开 / 关,空的写「没填」
    fn value(s: &Set) -> String {
        match s.switch {
            Some(true) => t!("开", "on").to_string(),
            Some(false) => t!("关", "off").to_string(),
            None if s.shown.is_empty() => t!("没填", "empty").to_string(),
            None => s.shown.clone(),
        }
    }

    /// 设置那一段:名字、值对齐成两列,改过的标出来,说明跟在后面(窄了不写)
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
                    let note = if s.secret { t!("原来的不显示", "old value hidden") } else { "" };
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
                    let v = kit::clip(&Self::value(s), val_w).trim_end().to_string();
                    let pad = val_w.saturating_sub(v.width());
                    let st = match (s.shown.is_empty() && s.switch.is_none(), s.changed) {
                        (true, _) => kit::dim(),
                        (false, true) => Style::new().add_modifier(Modifier::BOLD),
                        (false, false) => Style::new(),
                    };
                    spans.push(Span::styled(v, st));
                    spans.push(Span::raw(format!("{}  ", " ".repeat(pad))));
                    // 「改过」一列:没改过的空着,宽度照显示宽度算(中文一个字占两格)
                    let tag = t!("改过", "changed");
                    let shown = if s.changed { tag.to_string() } else { " ".repeat(tag.width()) };
                    spans.push(Span::styled(format!("{shown}  "), theme::accent()));
                    let used: usize = spans.iter().map(|x| x.content.width()).sum();
                    if w > 56 && used + 4 < w {
                        spans.push(Span::styled(kit::clip(&s.desc, w - used - 1).trim_end().to_string(), kit::dim()));
                    }
                }
            }
            out.push(pick(spans, w, on));
        }
    }

    fn tool_line(t: &Tool, w: usize, on: bool) -> Line<'static> {
        let (mark, st) = if t.on { ("✓", theme::accent().add_modifier(Modifier::BOLD)) } else { ("○", kit::dim()) };
        let right = if t.ask && !t.on { t!("要你点名", "only if allowed") } else { "" };
        let room = w.saturating_sub(right.width() + 8);
        let title = kit::clip(&t.title, room).trim_end().to_string();
        let id = kit::clip(&format!("  {}", t.id), room.saturating_sub(title.width())).trim_end().to_string();
        let line = kit::spread(
            vec![Span::raw("  "), Span::styled(mark, st), Span::raw(" "), Span::raw(title), Span::styled(id, kit::dim())],
            vec![Span::styled(right.to_string(), theme::warn())],
            w,
        );
        pick(line.spans, w, on)
    }

    fn bind_line(&self, b: &Bind, w: usize, on: bool) -> Line<'static> {
        if on && self.waiting {
            let text = tr!("按一个键给「{}」,Esc 不换", "press a key for “{}”, Esc keeps it", b.title);
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
            (Some(m), _) if m.is_empty() => t!("你去掉了键", "key removed by you").to_string(),
            (Some(_), Some(d)) => tr!("你改的(原来 {d})", "yours (was {d})"),
            (Some(_), None) => t!("你加的", "added by you").to_string(),
            (None, Some(d)) if b.key != Some(d) => tr!("{d} 被别的插件拿了", "{d} taken by another plugin"),
            (None, None) => t!("没有键", "no key").to_string(),
            (None, _) => String::new(),
        };
        let room = w.saturating_sub(right.width() + 8);
        let text = kit::clip(&b.title, room).trim_end().to_string();
        let line = kit::spread(vec![Span::raw("  "), key, Span::raw("  "), Span::raw(text)], vec![Span::styled(right, kit::dim())], w);
        pick(line.spans, w, on)
    }

    /// 这一栏的行;focused = 高亮在这一栏。也给出高亮那行是第几行(行多了跟着它滚)
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
            out.push(heading(
                t!("给 agent 的工具", "Tools for agents"),
                t!("改了,开着的会话重启才带上", "running sessions pick changes up after a restart"),
                w,
            ));
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
            let note = if d.enabled {
                t!("会话列表上按 : 打开,再按这里的键直接跑", "press : on the session list, then the key runs it")
            } else {
                t!("停着的插件不进菜单", "a paused plugin isn't in the menu")
            };
            out.push(heading(t!("插件菜单", "Plugin menu"), note, w));
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

    /// 高亮在这一栏时,底下写的键
    pub fn keys(&self) -> Vec<(&'static str, &'static str)> {
        if self.waiting {
            return vec![("Esc", t!("不换", "keep"))];
        }
        if self.input.is_some() {
            return vec![("⏎", t!("存", "save")), ("^U", t!("清空", "clear")), ("Esc", t!("不改", "cancel"))];
        }
        match self.at() {
            Some(At::Set(i)) if self.data.settings[i].switch.is_some() => {
                vec![(t!("空格", "space"), t!("切换", "toggle")), ("-", t!("默认", "default"))]
            }
            Some(At::Set(_)) => vec![("⏎", t!("改", "edit")), ("-", t!("默认", "default"))],
            Some(At::Tool(_)) => vec![(t!("空格", "space"), t!("给/不给", "give/withhold"))],
            Some(At::Bind(_)) => vec![("⏎", t!("换键", "set key")), ("⌫", t!("不要键", "no key")), ("-", t!("原来的", "manifest's"))],
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
            match (k.code, self.at()) {
                (KeyCode::Char(c), Some(At::Bind(i))) if !ctrl => {
                    let b = self.data.binds[i].id.clone();
                    self.job(Act::Key(id, b, Some(c.to_string())), fx);
                }
                _ => fx.say(t!("没换", "unchanged")),
            }
            return Back::Stay;
        }
        if let Some(text) = &mut self.input {
            match k.code {
                KeyCode::Esc => {
                    self.input = None;
                    fx.say(t!("没改", "unchanged"));
                }
                KeyCode::Enter => {
                    let v = std::mem::take(text);
                    self.input = None;
                    if let Some(At::Set(i)) = self.at() {
                        let s = &self.data.settings[i];
                        // secret 的从空的开始填:什么都没填就是不改
                        if s.secret && v.is_empty() {
                            fx.say(t!("没改", "unchanged"));
                        } else {
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
                    fx.say(tr!("{} 已经是默认的", "{} is already the default", s.name));
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
            info: vec![("事件".into(), "turn.done agent.waiting".into())],
            notes: vec!["键 e 给了 x「邮件」,aca.discord「邮件」没拿到".into()],
            log: true,
            mine: None,
        }
    }

    fn text(d: &Detail, w: usize, focused: bool) -> String {
        d.body(w, focused).0.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>().join("\n")
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
            // 设置:名字、值对齐,改过的标出来,开关写开 / 关,secret 打码;高亮那行行首一道竖条
            assert!(t.contains("▌ url       ••••abcd  改过  webhook 地址"), "{t}");
            assert!(t.contains("  workers   关") && t.contains("worker 也发"), "{t}");
            assert!(
                t.contains("给 agent 的工具  改了,开着的会话重启才带上") && t.contains("✓ 列出挂载  list") && t.contains("要你点名"),
                "{t}"
            );
            // 插件菜单:说清楚是会话列表上按 : 出来的那个
            assert!(t.contains("插件菜单  会话列表上按 : 打开,再按这里的键直接跑"), "{t}");
            assert!(t.contains("m  挂载") && t.contains("你改的(原来 t)") && t.contains("e 被别的插件拿了"), "{t}");
            assert!(t.contains("⚠ 键 e 给了") && t.contains("事件  turn.done agent.waiting"), "{t}");
            assert_eq!(d.keys()[0], ("⏎", "改"));
            // 高亮不在这一栏:哪行都不标
            let (_, hl) = d.body(84, false);
            assert!(hl.is_none() && !text(&d, 84, false).contains('▌'));
            // 窄屏(手机)不出界,说明不写
            assert!(d.body(40, true).0.iter().all(|l| l.width() <= 40), "{}", text(&d, 40, true));
            assert!(!text(&d, 40, true).contains("webhook 地址"));
        });
    }

    #[test]
    fn enter_edits_a_setting_space_flips_a_switch_and_minus_goes_back_to_the_default() {
        i18n::with(Lang::Zh, || {
            let mut d = Detail::new(fake("aca.discord"));
            // secret 的:从空的开始填,说原来的不显示;什么都不填 = 不改
            press(&mut d, KeyCode::Enter);
            assert!(d.typing() && text(&d, 84, true).contains("> ▏") && text(&d, 84, true).contains("原来的不显示"));
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
            // 开关:空格切换;没改过的按 - 说已经是默认
            press(&mut d, KeyCode::Char('j'));
            assert_eq!(
                job(press(&mut d, KeyCode::Char(' '))),
                Some(Act::Setting("aca.discord".into(), "workers".into(), Some("true".into())))
            );
            let fx = press(&mut d, KeyCode::Char('-'));
            assert!(fx.job.is_none() && fx.flash.unwrap().contains("已经是默认"));
            // 普通的:先填上现在的值,改了存;打字时 q 是字,不是回列表
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
            assert!(d.typing() && text(&d, 84, true).contains("按一个键给「挂载」,Esc 不换"));
            assert_eq!(job(press(&mut d, KeyCode::Char('q'))), Some(Act::Key("aca.tailnet".into(), "mounts".into(), Some("q".into()))));
            assert_eq!(job(press(&mut d, KeyCode::Backspace)), Some(Act::Key("aca.tailnet".into(), "mounts".into(), Some(String::new()))));
            assert_eq!(job(press(&mut d, KeyCode::Char('-'))), Some(Act::Key("aca.tailnet".into(), "mounts".into(), None)));
            // Esc 回列表;重读同一个插件高亮还在原来那行
            d.swap(fake("aca.tailnet"));
            assert_eq!(d.keys()[0], ("⏎", "换键"));
            assert_eq!(d.on_key(KeyEvent::from(KeyCode::Esc), &mut Fx::default()), Back::Leave);
        });
    }
}
