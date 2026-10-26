//! 起会话时替你答「信不信任这个目录」(`[trust] folder = true`,默认不替你答)。
//!
//! Claude、Codex 头一回进一个目录都会先问这一句,`--dangerously-skip-permissions` / `--yolo` 都不管它;
//! 没人答,会话就停在那,派出去的 worker 也一样。两家都没有「这个目录我信」的启动参数(Claude 的文档只说手改
//! `.claude.json`,那个文件同账号开着的会话一直在重写),所以照人的做法来:认出那个框,按键选「信任」,
//! 记账的事还是它自己做 —— 同一个账号下次进这个目录不再问。
//!
//! aca 没有常驻进程:[`crate::session`] 每起一次 agent 就放一个不等它的 [`spawn`],盯这个窗格半分钟,
//! 答完、或者 agent 已经报了 hook(启动那几道关过了)、或者到时间就退。只认 [`Agent::trust_keys`] 认得的那一个框,
//! 别的关(审 hook、更新)不碰。
//!
//! [`Agent::trust_keys`]: crate::agent::Agent::trust_keys

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::paths::{self, Res};
use crate::store::{Session, Store};
use crate::{agent, runtime, tmux};

/// 盯多久:慢的机器上 agent 画出那个框要几秒
const WATCH: Duration = Duration::from_secs(30);

/// 开关开着就放一个 `aca trust-watch <会话>` 在后台(不等它,起不来也不算启动失败)
pub fn spawn(cfg: &Config, id: &str) {
    if !cfg.trust.folder {
        return;
    }
    let _ = crate::proc::detach(
        Command::new(agent::self_exe()).args(["trust-watch", id]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()),
    );
}

/// 这个会话现在停在「信不信任这个目录」上,而且 [`spawn`] 放的那个后手还在盯着:马上有人替你答,不算在等你。
/// `aca wait` 问这个 —— 起完会话紧接着就等的(派活都这么写),不然会在框出来到被答掉的那半秒里等到一个「在等确认」
pub fn answering(cfg: &Config, s: &Session) -> bool {
    if !cfg.trust.folder || paths::now().saturating_sub(runtime::load(&s.id).started) > WATCH.as_secs() {
        return false;
    }
    let Some(p) = tmux::find(&s.id).filter(|p| !p.dead) else { return false };
    agent::of(cfg, s).is_ok_and(|(agent, _)| agent.trust_keys(&tmux::capture(&p.pane)).is_some())
}

/// `aca trust-watch <会话>`:那个框出来就按键。返回按没按
pub fn watch(store: &Store, cfg: &Config, key: &str) -> Res<bool> {
    let s = store.get(key)?;
    let (agent, _) = agent::of(cfg, &s)?;
    let (t, since) = (Instant::now(), paths::now());
    while t.elapsed() < WATCH {
        std::thread::sleep(Duration::from_millis(250));
        // 窗格还没换成 agent(占位的那一下)也接着等
        let Some(p) = tmux::find(&s.id).filter(|p| !p.dead) else { continue };
        let screen = tmux::capture(&p.pane);
        if let Some(keys) = agent.trust_keys(&screen) {
            // 画到一半的框不按:隔一会儿再看还是它才按
            std::thread::sleep(Duration::from_millis(200));
            if tmux::capture(&p.pane) != screen {
                continue;
            }
            for k in keys {
                tmux::run(&["send-keys", "-t", &p.pane, k])?;
            }
            return Ok(true);
        }
        // 它已经报过 hook 了:启动那几道关过了,这个框不会再出来
        let r = runtime::load(&s.id);
        if !r.event.is_empty() && r.ts >= since {
            break;
        }
    }
    Ok(false)
}
