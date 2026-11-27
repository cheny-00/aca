//! External channels in settings. Both pages switch the plugin itself, including its timers.

use std::path::Path;

use aca_core::{plugin, t, tr};
use ratatui::text::Span;

use crate::ops::Job;
use crate::overlay::{Fx, Setting};
use crate::plugins::Act;

struct Channel {
    id: String,
    name: String,
    on: bool,
    missing: Vec<String>,
}

fn installed() -> Vec<Channel> {
    plugin::first_party::refresh();
    plugin::registry()
        .into_iter()
        .filter_map(|e| {
            let (m, _) = plugin::read_manifest(Path::new(&e.path)).ok()?;
            let spec = m.channel.as_ref()?;
            let values = plugin::decl::settings(&e.id, &m);
            let missing = spec
                .settings
                .iter()
                .filter_map(|req| {
                    if req.split('|').any(|k| values.get(k).is_some_and(|v| v.as_str().is_none_or(|s| !s.trim().is_empty()))) {
                        return None;
                    }
                    Some(
                        req.split('|')
                            .map(|k| {
                                let label = &m.settings[k].label;
                                if label.is_empty() { k } else { label }
                            })
                            .collect::<Vec<_>>()
                            .join(t!("或", " or ")),
                    )
                })
                .collect();
            Some(Channel { id: e.id, name: spec.name.clone(), on: e.enabled, missing })
        })
        .collect()
}

pub struct Channels(fn() -> Vec<Channel>);

impl Channels {
    pub fn real() -> Self {
        Self(installed)
    }
}

impl Setting for Channels {
    fn key(&self) -> char {
        'c'
    }
    fn name(&self) -> &'static str {
        t!("外部渠道", "channels")
    }
    fn mark(&self) -> &'static str {
        ""
    }

    fn value(&self) -> Vec<Span<'static>> {
        let mut spans = vec![];
        for c in (self.0)() {
            if !spans.is_empty() {
                spans.push(Span::raw("  "));
            }
            let state = if c.on { t!("开", "on") } else { t!("关", "off") };
            spans.push(Span::raw(format!("{} {state}", c.name)));
            if c.on && !c.missing.is_empty() {
                spans.push(Span::styled(tr!(" · 缺 {}", " · missing {}", c.missing.join(t!("、", ", "))), crate::theme::warn()));
            }
        }
        spans
    }

    fn enter(&self, fx: &mut Fx) {
        let channels = (self.0)();
        let on = !channels.iter().any(|c| c.on);
        let jobs: Vec<_> = channels.into_iter().filter(|c| c.on != on).map(|c| Job::Plugins(Act::Enable(c.id, on))).collect();
        if !jobs.is_empty() {
            fx.job = Some(Job::Many(jobs));
        }
    }
}

pub fn switched(on: bool) -> String {
    if !on {
        return t!("外部渠道已关闭", "channels switched off").into();
    }
    enabled_note(&installed().into_iter().filter(|c| c.on).collect::<Vec<_>>())
}

fn enabled_note(channels: &[Channel]) -> String {
    let names = channels.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(t!("、", ", "));
    let missing: Vec<_> = channels
        .iter()
        .filter(|c| !c.missing.is_empty())
        .map(|c| {
            let what = c.missing.join(t!("、", ", "));
            if channels.len() == 1 { what } else { format!("{}: {what}", c.name) }
        })
        .collect();
    if missing.is_empty() {
        tr!("外部渠道已开启:{names}", "channels switched on: {names}")
    } else {
        tr!(
            "{names} 已开启,还缺 {} · 回列表按 P 填",
            "{names} switched on, missing {} · press P in the list to fill in",
            missing.join(" · ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discord(on: bool) -> Channel {
        Channel { id: "aca.discord".into(), name: "Discord".into(), on, missing: vec!["Bot token".into(), "收话的频道".into()] }
    }

    #[test]
    fn value_toggle_and_result_follow_installed_channels() {
        aca_core::i18n::with(aca_core::i18n::Lang::Zh, || {
            let off = Channels(|| vec![discord(false)]);
            let on = Channels(|| vec![discord(true), Channel { id: "me.chat".into(), name: "Chat".into(), ..discord(false) }]);
            let mut fx = Fx::default();
            assert_eq!((off.key(), off.name(), off.mark()), ('c', "外部渠道", ""));
            assert_eq!(off.value(), [Span::raw("Discord 关")]);
            off.enter(&mut fx);
            assert_eq!(fx.job.take(), Some(Job::Many(vec![Job::Plugins(Act::Enable("aca.discord".into(), true))])));
            on.enter(&mut fx);
            assert_eq!(fx.job.take(), Some(Job::Many(vec![Job::Plugins(Act::Enable("aca.discord".into(), false))])));
            let spans = on.value();
            assert_eq!(spans[1].style, crate::theme::warn());
            assert_eq!(ratatui::text::Line::from(spans).to_string(), "Discord 开 · 缺 Bot token、收话的频道  Chat 关");
            assert_eq!(enabled_note(&[discord(true)]), "Discord 已开启,还缺 Bot token、收话的频道 · 回列表按 P 填");
            assert_eq!(enabled_note(&[Channel { missing: vec![], ..discord(true) }]), "外部渠道已开启:Discord");
            let empty = Channels(Vec::new);
            assert!(empty.value().is_empty());
            empty.enter(&mut fx);
            assert!(fx.job.is_none());
            assert_eq!(switched(false), "外部渠道已关闭");
        });
    }
}
