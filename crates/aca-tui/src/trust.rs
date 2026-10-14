//! `^S` 设置页里「自动信任目录」那一行:`[trust] folder`(开关在 aca-core 的 config.rs,替你答的那一步在它的 trust.rs)。

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use aca_core::t;

use crate::overlay::{Fx, Setting};

pub struct TrustRow {
    on: fn() -> bool,
    set: fn(&str, &str, toml::Value) -> Result<(), String>,
}

impl TrustRow {
    pub fn real() -> TrustRow {
        TrustRow { on: || aca_core::config::load().trust.folder, set: crate::config::set }
    }
}

impl Setting for TrustRow {
    fn key(&self) -> char {
        'f'
    }

    fn name(&self) -> &'static str {
        t!("自动信任目录", "auto-trust folders")
    }

    fn value(&self) -> Vec<Span<'static>> {
        vec![Span::styled(if (self.on)() { t!("开", "on") } else { t!("关", "off") }, Style::new().add_modifier(Modifier::BOLD))]
    }

    fn mark(&self) -> &'static str {
        ""
    }

    fn enter(&self, fx: &mut Fx) {
        let on = !(self.on)();
        if let Err(e) = (self.set)("trust", "folder", toml::Value::Boolean(on)) {
            return fx.say(e);
        }
        fx.say(if on {
            t!(
                "新起、重启的会话:Claude / Codex 问「信不信任这个目录」时替你选信任",
                "new and restarted sessions: \"trust this folder?\" from Claude / Codex is answered for you"
            )
        } else {
            t!(
                "不替你答了:头一回进一个目录,会话停在那等你选",
                "not answered for you any more: a session waits for you the first time in a folder"
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_shows_the_switch_and_enter_writes_the_other_value() {
        use std::sync::Mutex;
        static WROTE: Mutex<Vec<(String, String, toml::Value)>> = Mutex::new(vec![]);
        let row = TrustRow {
            on: || false,
            set: |s, k, v| {
                WROTE.lock().unwrap().push((s.into(), k.into(), v));
                Ok(())
            },
        };
        assert_eq!((row.key(), row.mark()), ('f', ""));
        let shown: String = row.value().iter().map(|s| s.content.to_string()).collect();
        assert!(shown == "关" || shown == "off", "{shown}");
        let mut fx = Fx::default();
        row.enter(&mut fx);
        assert_eq!(*WROTE.lock().unwrap(), [("trust".to_string(), "folder".to_string(), toml::Value::Boolean(true))]);
        assert!(fx.flash.is_some());
        // 写不进去:说原因,不装作开了
        let stuck = TrustRow { on: || false, set: |_, _, _| Err("没记下".into()) };
        let mut fx = Fx::default();
        stuck.enter(&mut fx);
        assert_eq!(fx.flash.as_deref(), Some("没记下"));
    }
}
