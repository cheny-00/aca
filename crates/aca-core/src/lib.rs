//! aca: one screen for all your agent sessions. The backend is tmux; registering, starting, state and plugins are all done
//! here, with no dependency on agent-deck.
//!
//! Layers, lowest first. A module uses its own layer and the ones below, never one above:
//! - base: `paths` (where things live, time, ids), `i18n` (which language to speak, `tr!`), `screen` (what a screen says:
//!   running, asking, numbered options)
//! - storage: `runtime` `store` `config`, and `tmux` (calls into tmux)
//! - adapters: `agent/`, one file per tool (its command line, hooks, screen, title, conversation record); `proc`: reading
//!   processes (recognizing the agent and the account), and background child processes nobody waits for
//! - environment: `sandbox` `worktree` `trust` `import`, and `schedule` (what an agent scheduled for itself)
//! - plugins: `event` (the event stream), `plugin/` (the plugin host), `gateway/` (the shared MCP registry). `event` and
//!   `plugin` use each other, as do `gateway` and `plugin`
//! - sessions: `launch` (putting a session's command together), `session` (operations shared by the interface and the
//!   CLI), `adopt` (sessions aca didn't start), `hook` `relay` `status` (where the state comes from; `relay`: what the
//!   agent reports itself, OSC 7501), `inbox` `answer` (messages between sessions, answering confirmations), `router`
//!   `thread` (a group's router: what a new one is told, and its exchanges read off the event stream). These call
//!   each other: there's no resident process, so looking at a session's state fills in the hook event the agent didn't
//!   report, which goes through the inbox, which looks at the state again
//!
//! This library doesn't parse the command line, read through what agents write down themselves (conversation text, usage,
//! quotas) or draw the interface: that's the job of the `aca` (CLI), `aca-records` and `aca-tui` (interface) crates, which
//! sit above it.

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
pub mod launch;
pub mod paths;
pub mod plugin;
pub mod proc;
pub mod relay;
pub mod router;
pub mod runtime;
pub mod sandbox;
pub mod schedule;
pub mod screen;
pub mod session;
pub mod status;
pub mod store;
pub mod task;
pub mod thread;
pub mod tmux;
pub mod trust;
pub mod worktree;
