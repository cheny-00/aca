//! A plain shell: just a tmux pane, with no hooks and no conversation.

use super::{Agent, How};
use crate::config::Tool;
use crate::paths::Res;
use crate::store::Session;

pub struct Shell;

impl Agent for Shell {
    fn name(&self) -> &str {
        "shell"
    }

    fn program(&self) -> String {
        std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())
    }

    fn argv(&self, tool: &Tool, s: &Session, _how: &How, _aca: &str) -> Res<Vec<String>> {
        let mut argv = super::base(tool, &self.program());
        argv.extend(super::extra(tool, s));
        Ok(argv)
    }

    /// No process counts as "the shell tool running": shell is what's left when no agent is recognized
    fn runs_as(&self, _comm: &str) -> bool {
        false
    }

    /// Messages from other sessions aren't typed into a shell: typed in, they would become a command
    fn delivery(&self) -> super::Delivery {
        super::Delivery::Keep
    }
}
