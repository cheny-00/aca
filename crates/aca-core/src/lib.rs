//! aca:一屏管住所有 agent 会话。后端是 tmux,会话的登记、启动、状态、插件都自己做,不依赖 agent-deck。
//!
//! 分层(下面的不知道上面的):
//! - `paths` `tmux` `store` `config` `runtime`:存东西和调 tmux 的地方
//! - `agent/`:各家 agent 的适配(怎么起、hook、屏幕、标题),一家一个文件
//! - `screen` `hook` `status`:状态怎么来
//! - `session`:对会话的操作,界面和命令行共用;`adopt`:接管不是 aca 起的会话;`proc`:读进程(认 agent、认账号)
//! - `event` `plugin`:事件流和插件宿主
//! - `i18n`:说哪种话(`tr!`),命令行和界面共用
//! - `inbox` `answer`:会话之间递话(收件箱、门铃、排队、自动通知、`aca wait`),替在等确认的会话回答
//!
//! 这个库不解析命令行、不画界面:那是 `aca`(命令行)和以后的界面那两个 crate 的事。

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
pub mod screen;
pub mod session;
pub mod status;
pub mod store;
pub mod tmux;
pub mod trust;
pub mod worktree;
