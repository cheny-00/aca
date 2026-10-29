//! aca: one screen for all your agent sessions. The backend is tmux; registering, starting, state and plugins are all done
//! here, with no dependency on agent-deck.
//!
//! Layers (lower ones don't know about the ones above):
//! - `paths` `tmux` `store` `config` `runtime`: storage and calls into tmux
//! - `agent/`: adapters for each agent (how to start it, hooks, screen, title), one file per tool
//! - `screen` `hook` `status`: where the state comes from
//! - `session`: operations on sessions, shared by the interface and the CLI; `adopt`: adopting sessions aca didn't start;
//!   `proc`: reading processes (recognizing the agent and the account), and background child processes nobody waits for
//! - `event` `plugin`: the event stream and the plugin host
//! - `i18n`: which language to speak (`tr!`), shared by the CLI and the interface
//! - `inbox` `answer`: passing messages between sessions (inbox, doorbell, queueing, automatic notices, `aca wait`), and
//!   answering for sessions waiting for a confirmation
//!
//! This library doesn't parse the command line or draw the interface: that's the job of the `aca` (CLI) crate and the
//! interface crate that comes later.

pub mod adopt;
pub mod agent;
pub mod answer;
pub mod config;
pub mod event;
pub mod gateway;
pub mod hook;
pub mod i18n;
pub mod import;
pub mod inbox;
pub mod paths;
pub mod plugin;
pub mod proc;
pub mod runtime;
pub mod schedule;
pub mod screen;
pub mod session;
pub mod status;
pub mod store;
pub mod tmux;
pub mod trust;
pub mod worktree;
