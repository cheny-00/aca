//! Reading what each agent writes down itself: conversation records, usage and what it costs, quotas, the directory a
//! session really works in. Nothing here draws: the interface (`aca-tui`) and the command line (`aca`) both read through it.

pub mod agent;
pub mod dialog;
pub mod limits;
pub mod paths;
pub mod slots;
pub mod statusline;
pub mod subagent;
pub mod trace;
pub mod trace_claude;
pub mod trace_codex;
pub mod usage;
pub mod words;
pub mod workdir;
