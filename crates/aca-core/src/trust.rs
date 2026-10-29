//! Answer "do you trust this folder?" for you when a session starts (`[trust] folder = true`; by default it isn't answered for you).
//!
//! Claude and Codex both ask this first thing the first time in a folder, and neither `--dangerously-skip-permissions` nor
//! `--yolo` skips it; with nobody to answer, the session just sits there, and so does a delegated worker. Neither tool has a
//! launch argument for "I trust this folder" (Claude's docs only say to edit `.claude.json` by hand, a file that open sessions of
//! the same account keep rewriting), so do what a person does: recognize the prompt and press the key to choose "trust", leaving
//! the bookkeeping to the tool itself — the same account doesn't ask again next time in this folder.
//!
//! aca has no resident process: each time [`crate::session`] starts an agent, it puts out a [`spawn`] it doesn't wait for, which
//! watches the pane for half a minute and exits once it has answered, or the agent has reported a hook (the startup gates are
//! past), or time is up. It only recognizes the one prompt [`Agent::trust_keys`] knows; other gates (reviewing hooks, updates)
//! are left alone.
//!
//! [`Agent::trust_keys`]: crate::agent::Agent::trust_keys

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::paths::{self, Res};
use crate::store::{Session, Store};
use crate::{agent, runtime, tmux};

/// How long to watch: on a slow machine the agent takes a few seconds to draw the prompt
const WATCH: Duration = Duration::from_secs(30);

/// With the switch on, put an `aca trust-watch <session>` in the background (not waited for; failing to start doesn't count as
/// the start failing)
pub fn spawn(cfg: &Config, id: &str) {
    if !cfg.trust.folder {
        return;
    }
    let _ = crate::proc::detach(
        Command::new(agent::self_exe()).args(["trust-watch", id]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()),
    );
}

/// This session is sitting at "do you trust this folder?" right now, and the follow-up [`spawn`] put out is still watching: someone
/// will answer for you in a moment, so it doesn't count as needing you. `aca wait` asks this — waiting right after starting a
/// session (delegating is always written that way) would otherwise catch "waiting for a confirmation" in the half second between
/// the prompt appearing and it being answered
pub fn answering(cfg: &Config, s: &Session) -> bool {
    if !cfg.trust.folder || paths::now().saturating_sub(runtime::load(&s.id).started) > WATCH.as_secs() {
        return false;
    }
    let Some(p) = tmux::find(&s.id).filter(|p| !p.dead) else { return false };
    agent::of(cfg, s).is_ok_and(|(agent, _)| agent.trust_keys(&tmux::capture(&p.pane)).is_some())
}

/// `aca trust-watch <session>`: press the key once that prompt appears. Returns whether it pressed
pub fn watch(store: &Store, cfg: &Config, key: &str) -> Res<bool> {
    let s = store.get(key)?;
    let (agent, _) = agent::of(cfg, &s)?;
    let (t, since) = (Instant::now(), paths::now());
    while t.elapsed() < WATCH {
        std::thread::sleep(Duration::from_millis(250));
        // Keep waiting while the pane hasn't switched to the agent yet (the placeholder moment)
        let Some(p) = tmux::find(&s.id).filter(|p| !p.dead) else { continue };
        let screen = tmux::capture(&p.pane);
        if let Some(keys) = agent.trust_keys(&screen) {
            // Don't press on a half-drawn prompt: only press if it's still the same a moment later
            std::thread::sleep(Duration::from_millis(200));
            if tmux::capture(&p.pane) != screen {
                continue;
            }
            for k in keys {
                tmux::run(&["send-keys", "-t", &p.pane, k])?;
            }
            return Ok(true);
        }
        // It has already reported a hook: the startup gates are past, and this prompt won't show again
        let r = runtime::load(&s.id);
        if !r.event.is_empty() && r.ts >= since {
            break;
        }
    }
    Ok(false)
}
