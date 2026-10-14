//! 普通 shell:只是 tmux 里开一格,没有 hook、没有对话。

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

    /// 哪个进程都不算「在跑的 shell 这一家」:shell 是没认出 agent 时剩下的那个
    fn runs_as(&self, _comm: &str) -> bool {
        false
    }

    /// 别的会话发来的消息不往 shell 里打:打进去就成了一条命令
    fn delivery(&self) -> super::Delivery {
        super::Delivery::Keep
    }
}
