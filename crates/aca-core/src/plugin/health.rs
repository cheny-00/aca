//! 插件出了什么错:事件钩子、定时上次没跑成,gateway 下面的服务连着失败停下了。
//! tmux 状态栏(`aca tmux-status`)、界面顶栏、插件页、`aca plugin ls` 都读这里。
//!
//! - 钩子:起的时候外面包一层 sh([`WRAP`]),跑完把退出码写进 `state/plugin-health/<插件>/hook-<第几个>`
//!   (钩子不等,aca 早走了;时刻是那个文件的修改时间)。清单换了(重新登记)就清掉:第几个对不上了
//! - 定时:[`super::timer`] 自己记着上次的退出码
//! - gateway:问在跑的 gateway(最多等一小会儿);没在跑就没什么可说的
//!
//! 只算开着的插件;上次没成、[`RECENT`] 以内的才算 —— 下次跑成了、插件停了、过了一天都不再提。

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{read_manifest, registry};
use crate::paths;
use crate::tr;

/// 多久以前的不再提
pub const RECENT: u64 = 24 * 3600;
/// 问 gateway 最多等这么久(状态栏每隔几秒问一次,不能被它拖住)
const GATEWAY_WAIT: Duration = Duration::from_millis(300);

/// 包在钩子外面的 sh:`sh -c WRAP sh <记录的文件> <命令> <参数…>`。跑完把退出码写进那个文件,退出码照原样交出去
pub(crate) const WRAP: &str = r#"f=$1; shift; "$@"; rc=$?; printf '%s\n' "$rc" >"$f.$$" && mv -f "$f.$$" "$f"; exit $rc"#;

/// 一个插件的一处毛病
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub plugin: String,
    /// 哪一处:「钩子 turn.done」「定时 collect」「服务 web」
    pub what: String,
    /// 怎么了:「退出码 3」、为什么
    pub detail: String,
    /// 什么时候(秒);gateway 的是现在
    pub at: u64,
}

impl Problem {
    /// 「钩子 turn.done 退出码 3 · 5m 前」
    pub fn text(&self, now: u64) -> String {
        let ago = super::timer::span(now.saturating_sub(self.at));
        tr!("{} {} · {ago} 前", "{} {} · {ago} ago", self.what, self.detail)
    }
}

fn dir(plugin: &str) -> PathBuf {
    paths::state_dir().join("plugin-health").join(plugin)
}

/// 第几个事件钩子(清单里 `[[events]]` 的顺序,从 0 数)跑完记在哪
pub(crate) fn hook_file(plugin: &str, index: usize) -> PathBuf {
    dir(plugin).join(format!("hook-{index}"))
}

/// 插件重新登记 / 不用了:它的钩子记录对不上了,清掉
pub(crate) fn clear(plugin: &str) {
    let _ = std::fs::remove_dir_all(dir(plugin));
}

fn exit_text(code: i32) -> String {
    match code {
        127 => tr!("无法启动(退出码 127:找不到命令)", "didn't start (exit code 127: command not found)"),
        c => tr!("退出码 {c}", "exit code {c}"),
    }
}

/// 开着的插件现在的毛病,先插件后哪一处。状态栏每隔几秒问一次:没有记录的插件连清单都不读
pub fn problems() -> Vec<Problem> {
    let now = paths::now();
    let recent = |at: u64| now.saturating_sub(at) <= RECENT;
    let timers = super::timer::all();
    let mut out = vec![];
    for e in registry().into_iter().filter(|e| e.enabled) {
        let gw = e.id == crate::gateway::PLUGIN;
        let failed_timer = timers.iter().any(|s| s.plugin == e.id && s.last > 0 && s.rc != Some(0) && recent(s.last));
        // 钩子:第几个 → (退出码, 时刻),只留没成的
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

/// gateway 下面连着失败停下了的服务(要 `aca plugin gateway restart` 或者改配置才会再起)
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

    /// 包在钩子外面的那层:退出码照原样交出去,也记进文件;命令的参数原样传
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
