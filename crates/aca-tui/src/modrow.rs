//! The `m` row on the settings page (Claude's mod on / off, and whether this Claude version loads it), and the line atop the
//! billing panel's quotas saying the mod is on but not loaded. The quotas themselves are in `aca_records::limits`.

use aca_core::agent::claude_mod::Verdict;
use aca_core::config::Tool;
use aca_core::{t, tr};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::overlay::{Fx, Setting};

/// The line at the top of the billing panel's quota section: the mod is on (`[tools.<the claude one>] mod = true`) but isn't
/// loaded on the current Claude version.
/// Off, loaded, or no session started yet (unverified) = empty. Why it isn't loaded is written on the mod row of the settings page
pub fn mod_note(cfg: &aca_core::config::Config) -> String {
    let on = aca_core::agent::names(cfg)
        .iter()
        .filter_map(|n| aca_core::agent::tool(cfg, n.split(':').next()?))
        .any(|t| t.kind == "claude" && t.mods);
    match on.then(aca_core::agent::claude_mod::verdict).flatten() {
        Some(v) if !v.ok => t!("额度可能不是最新数据", "quotas may be out of date").into(),
        _ => String::new(),
    }
}

/// The read/write hooks for the mod row in `^S` (swapped for fakes in tests)
#[derive(Clone, Copy)]
pub struct ModIo {
    /// How `[tools.claude]` is written now
    tool: fn() -> Tool,
    /// Whether the current Claude version loads it (unverified = None)
    verdict: fn() -> Option<Verdict>,
    set: fn(&str, &str, toml::Value) -> Result<(), String>,
}

/// A row on the settings page: "m  Claude mod  on", followed by why if it's on but has no effect. ⏎ toggles it on the
/// spot, changing `[tools.claude] mod`;
/// open sessions are untouched, they follow on their next start (new, restart)
pub struct ModRow {
    io: ModIo,
}

impl ModRow {
    pub fn real() -> ModRow {
        ModRow {
            io: ModIo {
                tool: || aca_core::agent::tool(&aca_core::config::load(), "claude").unwrap_or_default(),
                verdict: aca_core::agent::claude_mod::verdict,
                set: crate::config::set,
            },
        }
    }

    /// On but with no effect: the why written in brackets on the row and said in the bottom bar when turning it on.
    /// Loadable (unverified counts) = None
    fn not_carried(&self, tool: &Tool) -> Option<(String, String)> {
        // Wrapped in a command of its own: no telling which Claude version runs inside, so not loaded
        if !tool.cmd.is_empty() {
            return Some((
                t!("已自定义命令", "custom command").into(),
                t!("已自定义命令,mod 不生效", "a custom command is set; the mod has no effect").into(),
            ));
        }
        match (self.io.verdict)() {
            Some(v) if !v.ok => {
                Some((format!("Claude {}", v.build), tr!("Claude {} 未加载 mod:{}", "Claude {} won't load the mod: {}", v.build, v.why)))
            }
            _ => None,
        }
    }
}

impl Setting for ModRow {
    fn key(&self) -> char {
        'm'
    }

    fn name(&self) -> &'static str {
        "Claude mod"
    }

    fn value(&self) -> Vec<Span<'static>> {
        let tool = (self.io.tool)();
        let bold = Style::new().add_modifier(Modifier::BOLD);
        if !tool.mods {
            return vec![Span::styled(t!("关", "off"), bold)];
        }
        let mut out = vec![Span::styled(t!("开", "on"), bold)];
        if let Some((short, _)) = self.not_carried(&tool) {
            out.push(Span::styled(tr!(" · 不生效({short})", " · no effect ({short})"), crate::theme::warn()));
        }
        out
    }

    fn mark(&self) -> &'static str {
        ""
    }

    fn enter(&self, fx: &mut Fx) {
        let tool = (self.io.tool)();
        let on = !tool.mods;
        if let Err(e) = (self.io.set)("tools.claude", "mod", toml::Value::Boolean(on)) {
            return fx.say(e);
        }
        // Open sessions are untouched, only newly started / restarted ones follow
        fx.say(match self.not_carried(&tool) {
            Some((_, why)) if on => why,
            _ => t!("已保存,新会话生效", "saved; new sessions use it").into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        /// Fake settings row: how `[tools.claude]` is written now, this version's verdict, what was written to config.toml
        static TOOL: std::cell::RefCell<Tool> = std::cell::RefCell::new(Tool::default());
        static VERDICT: std::cell::RefCell<Option<Verdict>> = const { std::cell::RefCell::new(None) };
        static WROTE: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(vec![]) };
    }

    #[test]
    fn the_settings_row_flips_the_mod_and_says_whether_this_claude_takes_it() {
        let row = ModRow {
            io: ModIo {
                tool: || TOOL.with(|t| t.borrow().clone()),
                verdict: || VERDICT.with(|v| v.borrow().clone()),
                set: |sec, key, v| {
                    WROTE.with(|w| w.borrow_mut().push(format!("[{sec}] {key} = {v}")));
                    // Once written, the next read sees the new value
                    TOOL.with(|t| t.borrow_mut().mods = v.as_bool().unwrap());
                    Ok(())
                },
            },
        };
        let shown = |row: &ModRow| row.value().iter().map(|s| s.content.to_string()).collect::<String>();
        let enter = |row: &ModRow| {
            let mut fx = Fx::default();
            row.enter(&mut fx);
            fx.flash.unwrap()
        };
        assert_eq!((row.key(), row.mark(), shown(&row).as_str()), ('m', "", "关"));
        // On: Claude never started yet, unverified — shows just 「开」 ("on")
        assert_eq!(enter(&row), "已保存,新会话生效");
        assert_eq!(WROTE.with(|w| w.borrow_mut().drain(..).collect::<Vec<_>>()), ["[tools.claude] mod = true"]);
        assert_eq!(shown(&row), "开");
        // This version verified and loadable: still shows just 「开」 ("on")
        let verdict = |ok: bool, why: &str| Some(Verdict { build: "2.1.295".into(), ok, why: why.into(), at: 1 });
        VERDICT.with(|v| *v.borrow_mut() = verdict(true, ""));
        assert_eq!(shown(&row), "开");
        // This version doesn't load it: marked on the row; turn it off and on again, the bottom bar says why
        VERDICT.with(|v| *v.borrow_mut() = verdict(false, "它交来的用量认不出了"));
        assert_eq!((shown(&row).as_str(), row.value()[1].style), ("开 · 不生效(Claude 2.1.295)", crate::theme::warn()));
        assert_eq!(enter(&row), "已保存,新会话生效");
        assert_eq!(shown(&row), "关");
        assert_eq!(enter(&row), "Claude 2.1.295 未加载 mod:它交来的用量认不出了");
        assert_eq!(
            WROTE.with(|w| w.borrow_mut().drain(..).collect::<Vec<_>>()),
            ["[tools.claude] mod = false", "[tools.claude] mod = true"]
        );
        // The tool is wrapped in a command of its own: no version loads it
        TOOL.with(|t| t.borrow_mut().cmd = vec!["wrap.sh".into()]);
        assert_eq!(shown(&row), "开 · 不生效(已自定义命令)");
    }
}
