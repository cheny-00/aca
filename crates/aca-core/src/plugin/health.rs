//! What went wrong in plugins: an event hook or a timer whose last run failed, or a server under the gateway that kept
//! failing and was stopped. The tmux status bar (`aca tmux-status`), the interface's top bar, the plugins page and
//! `aca plugin ls` all read this.
//!
//! - Hooks: started wrapped in an sh ([`WRAP`]) that writes the exit code to `state/plugin-health/<plugin>/hook-<index>`
//!   when done (hooks aren't waited for, aca is long gone; the time is that file's modification time). Cleared when the
//!   manifest changes (re-registered): the indexes no longer match
//! - Timers: [`super::timer`] keeps the last exit code itself
//! - Gateway: ask the running gateway (waiting a short while at most); if it isn't running there's nothing to say
//!
//! Only enabled plugins count, and only last runs that failed within [`RECENT`] — once the next run succeeds, the plugin
//! is disabled, or a day has passed, it's no longer mentioned.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{read_manifest, registry};
use crate::paths;
use crate::tr;

/// Older than this is no longer mentioned
pub const RECENT: u64 = 24 * 3600;
/// How long to wait for the gateway at most (the status bar asks every few seconds and can't be held up by it)
const GATEWAY_WAIT: Duration = Duration::from_millis(300);

/// The sh wrapped around a hook: `sh -c WRAP sh <record file> <command> <args…>`. Writes the exit code to that file when
/// done, and passes the exit code on unchanged
pub(crate) const WRAP: &str = r#"f=$1; shift; "$@"; rc=$?; printf '%s\n' "$rc" >"$f.$$" && mv -f "$f.$$" "$f"; exit $rc"#;

/// One problem of a plugin
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub plugin: String,
    /// Where: "hook turn.done", "timer collect", "server web"
    pub what: String,
    /// What happened: "exit code 3", or why
    pub detail: String,
    /// When (seconds); for the gateway it's now
    pub at: u64,
}

impl Problem {
    /// "hook turn.done exit code 3 · 5m ago"
    pub fn text(&self, now: u64) -> String {
        let ago = super::timer::span(now.saturating_sub(self.at));
        tr!("{} {} · {ago} 前", "{} {} · {ago} ago", self.what, self.detail)
    }
}

fn dir(plugin: &str) -> PathBuf {
    paths::state_dir().join("plugin-health").join(plugin)
}

/// Where the event hook with this index (the order of `[[events]]` in the manifest, from 0) records its finished run
pub(crate) fn hook_file(plugin: &str, index: usize) -> PathBuf {
    dir(plugin).join(format!("hook-{index}"))
}

/// The plugin was re-registered / is gone: its hook records no longer match, clear them
pub(crate) fn clear(plugin: &str) {
    let _ = std::fs::remove_dir_all(dir(plugin));
}

fn exit_text(code: i32) -> String {
    match code {
        127 => tr!("无法启动(退出码 127:找不到命令)", "didn't start (exit code 127: command not found)"),
        c => tr!("退出码 {c}", "exit code {c}"),
    }
}

/// Current problems of enabled plugins, by plugin then by where. The status bar asks every few seconds: plugins without
/// records don't even get their manifest read
pub fn problems() -> Vec<Problem> {
    let now = paths::now();
    let recent = |at: u64| now.saturating_sub(at) <= RECENT;
    let timers = super::timer::all();
    let mut out = vec![];
    for e in registry().into_iter().filter(|e| e.enabled) {
        let gw = e.id == crate::gateway::PLUGIN;
        let failed_timer = timers.iter().any(|s| s.plugin == e.id && s.last > 0 && s.rc != Some(0) && recent(s.last));
        // Hooks: index → (exit code, time), only the failed ones kept
        let hooks: Vec<(usize, i32, u64)> = std::fs::read_dir(dir(&e.id))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|f| {
                let i = f.file_name().to_str()?.strip_prefix("hook-")?.parse::<usize>().ok()?;
                let code = std::fs::read_to_string(f.path()).ok()?.trim().parse::<i32>().ok()?;
                let at = f.metadata().ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
                (code != 0 && recent(at)).then_some((i, code, at))
            })
            .collect();
        if !gw && !failed_timer && hooks.is_empty() {
            continue;
        }
        let Ok((m, _)) = read_manifest(Path::new(&e.path)) else { continue };
        let mut hooks = hooks;
        hooks.sort();
        for (i, code, at) in hooks {
            let Some(h) = m.events.get(i) else { continue };
            out.push(Problem { plugin: e.id.clone(), what: tr!("钩子 {}", "hook {}", h.on), detail: exit_text(code), at });
        }
        for t in &m.timers {
            let Some(st) = timers.iter().find(|s| s.plugin == e.id && s.run == t.run) else { continue };
            if st.last > 0 && st.rc != Some(0) && recent(st.last) {
                let detail = match st.rc {
                    Some(c) => exit_text(c),
                    None => st.why.clone(),
                };
                out.push(Problem { plugin: e.id.clone(), what: tr!("定时 {}", "timer {}", t.run), detail, at: st.last });
            }
        }
        if gw {
            out.extend(gateway(&e.id, now));
        }
    }
    out
}

/// Servers under the gateway that kept failing and were stopped (they only start again after `aca plugin gateway restart`
/// or a config change)
fn gateway(plugin: &str, now: u64) -> Vec<Problem> {
    if !crate::gateway::sock_path().exists() {
        return vec![];
    }
    let Ok(v) = crate::gateway::ctl_within(serde_json::json!({ "cmd": "status" }), GATEWAY_WAIT) else { return vec![] };
    stopped(plugin, &v, now)
}

fn stopped(plugin: &str, status: &serde_json::Value, now: u64) -> Vec<Problem> {
    status["servers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["stopped"].as_bool() == Some(true))
        .map(|s| {
            let name = s["name"].as_str().unwrap_or("");
            let what = match s["session"].as_str().filter(|x| !x.is_empty()) {
                Some(sess) => tr!("服务 {name}(会话 {sess})", "server {name} (session {sess})"),
                None => tr!("服务 {name}", "server {name}"),
            };
            let err = s["error"].as_str().unwrap_or("");
            Problem {
                plugin: plugin.into(),
                what,
                detail: tr!(
                    "连续失败,已停止:{err}(aca plugin gateway restart {name})",
                    "failed repeatedly and stopped: {err} (aca plugin gateway restart {name})"
                ),
                at: now,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_problem_says_where_what_and_when() {
        crate::i18n::with(crate::i18n::Lang::Zh, || {
            let p = Problem { plugin: "aca.bark".into(), what: "钩子 turn.done".into(), detail: exit_text(3), at: 1000 };
            assert_eq!(p.text(1300), "钩子 turn.done 退出码 3 · 5m 前");
            assert!(exit_text(127).contains("找不到命令"));
        });
    }

    #[test]
    fn only_gateway_servers_that_gave_up_count() {
        let status = serde_json::json!({ "servers": [
            { "name": "web", "session": "", "state": "failed", "error": "退出了 (×5)", "stopped": true },
            { "name": "fs", "session": "", "state": "failed", "error": "退出了 (×2)", "stopped": false },
            { "name": "pw", "session": "ab12", "state": "failed", "error": "x (×5)", "stopped": true },
            { "name": "ok", "session": "", "state": "ready", "error": "" }
        ]});
        crate::i18n::with(crate::i18n::Lang::Zh, || {
            let got = stopped("aca.gateway", &status, 7);
            let whats: Vec<&str> = got.iter().map(|p| p.what.as_str()).collect();
            assert_eq!(whats, ["服务 web", "服务 pw(会话 ab12)"]);
            assert!(got[0].detail.contains("退出了 (×5)") && got[0].detail.contains("restart web"));
        });
    }

    /// The wrapper around a hook: the exit code is passed on unchanged and also written to the file; the command's arguments
    /// are passed as they are
    #[test]
    fn the_wrapper_keeps_the_exit_code_and_writes_it_down() {
        let d = std::env::temp_dir().join(format!("aca-health-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("hook-0");
        let st = std::process::Command::new("/bin/sh")
            .args(["-c", WRAP, "sh", f.to_str().unwrap(), "sh", "-c", "[ \"$1\" = 'a b' ] && exit 3", "x", "a b"])
            .status()
            .unwrap();
        assert_eq!(st.code(), Some(3));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "3\n");
        let st = std::process::Command::new("/bin/sh").args(["-c", WRAP, "sh", f.to_str().unwrap(), "true"]).status().unwrap();
        assert!(st.success());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "0\n");
        let _ = std::fs::remove_dir_all(&d);
    }
}
