//! Declared tools: no code, just a `[tools.<name>]` section in config.toml to plug one in (following agent-deck's
//! `[tools.X]`).
//!
//! ```toml
//! [tools.aider]
//! cmd = ["aider"]                     # defaults to the tool name
//! home_var = "AIDER_HOME"             # which env variable gets the account's home (config dir); else accounts can only use env / args
//! resume = ["--session", "{conv}"]    # resuming: {conv} becomes the conversation id
//! fork = ["--fork", "{conv}"]         # forking a conversation; without it, no forking
//! busy = ['Working\.\.\.']            # appears at the bottom of the screen = running (regexes, any one)
//! ask = ['\(y/n\)', 'Allow .*\?']     # appears at the bottom of the screen = needs you
//! title = "pane"                      # title from the pane title (leading spinner removed); without it only what the hook reports
//! env = { FOO = "1" }                 # environment variables added on start
//! ```
//!
//! With just these it can start, stop, restart in place, and the list shows running / needs you. Resuming a conversation
//! needs someone to report the conversation id: if the agent has its own plugin / extension mechanism, write a small
//! forwarder that runs `"$ACA_BIN" hook "$ACA_TOOL"` on events, with a hook-protocol JSON on stdin (shape: see
//! [`super::lifecycle`]), and the conversation id, the last reply, unread and the title all work.

use regex::Regex;

use super::{Agent, How};
use crate::config::Tool;
use crate::paths::Res;
use crate::runtime::{Run, clip};
use crate::screen::Seen;
use crate::store::Session;
use crate::tr;

pub struct Declared {
    name: String,
    home_var: Option<String>,
    resume: Vec<String>,
    fork: Vec<String>,
    busy: Vec<Regex>,
    ask: Vec<Regex>,
    pane_title: bool,
}

fn compile(tool: &str, field: &str, patterns: &[String]) -> Res<Vec<Regex>> {
    patterns
        .iter()
        .map(|p| {
            Regex::new(p).map_err(|e| {
                tr!("工具 {tool} 的 {field} 中 {p} 不是有效的正则表达式:{e}", "{p} in {field} of tool {tool} isn't a regex: {e}")
            })
        })
        .collect()
}

/// `["--session", "{conv}"]` → `["--session", "c-1"]`
fn fill(template: &[String], conv: &str) -> Vec<String> {
    template.iter().map(|a| a.replace("{conv}", conv)).collect()
}

/// Strip spinner / status characters at the start of the pane title: the braille block (⠋⠙…) and Claude's asterisks
fn strip_spinner(title: &str) -> &str {
    title.trim_start_matches(|c: char| c.is_whitespace() || ('\u{2800}'..='\u{28FF}').contains(&c) || "✳✶✻✽✢·*".contains(c)).trim_end()
}

impl Declared {
    pub fn new(name: &str, t: &Tool) -> Res<Declared> {
        if !matches!(t.title.as_str(), "" | "pane") {
            return Err(tr!("工具 {name} 的 title 只能写 \"pane\" 或省略", "tool {name}: title can only be \"pane\" or left out"));
        }
        Ok(Declared {
            name: name.to_string(),
            home_var: (!t.home_var.is_empty()).then(|| t.home_var.clone()),
            resume: t.resume.clone(),
            fork: t.fork.clone(),
            busy: compile(name, "busy", &t.busy)?,
            ask: compile(name, "ask", &t.ask)?,
            pane_title: t.title == "pane",
        })
    }
}

impl Agent for Declared {
    fn name(&self) -> &str {
        &self.name
    }

    fn home_var(&self) -> Option<&str> {
        self.home_var.as_deref()
    }

    fn can_fork(&self) -> bool {
        !self.fork.is_empty()
    }

    fn argv(&self, tool: &Tool, s: &Session, how: &How, _aca: &str) -> Res<Vec<String>> {
        let mut argv = super::base(tool, &self.name);
        match how {
            How::Fresh => {}
            How::Resume(conv) => argv.extend(fill(&self.resume, conv)),
            How::Fork(from) => argv.extend(fill(&self.fork, from)),
        }
        argv.extend(super::extra(tool, s));
        Ok(argv)
    }

    /// Resume if the conversation id is known and how to resume is set; a fork that hasn't reported an id yet forks again
    /// from the original
    fn resume(&self, conv: &str, r: &Run) -> How {
        if !conv.is_empty() && !self.resume.is_empty() {
            How::Resume(conv.to_string())
        } else if !r.fork_from.is_empty() && !self.fork.is_empty() {
            How::Fork(r.fork_from.clone())
        } else {
            How::Fresh
        }
    }

    fn title_in_pane(&self) -> bool {
        self.pane_title
    }

    /// Use the title if the hook reported one; if not, and the pane is the title source, take the pane title
    fn title(&self, r: &Run, pane_title: &str) -> Option<String> {
        if !r.hook_title.is_empty() {
            return Some(r.hook_title.clone());
        }
        let t = strip_spinner(pane_title);
        (self.pane_title && !t.is_empty()).then(|| t.to_string())
    }

    /// Only look at these bottom lines: words in the history scrolled up don't count
    fn screen(&self, lines: &[String]) -> Seen {
        let nonempty: Vec<&str> = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
        let tail = &nonempty[nonempty.len().saturating_sub(15)..];
        if let Some(l) = tail.iter().rev().find(|l| self.ask.iter().any(|re| re.is_match(l))) {
            return Seen::Ask(clip(l, 200));
        }
        if tail.iter().any(|l| self.busy.iter().any(|re| re.is_match(l))) {
            // How long it's been running isn't necessarily on screen: the list uses the turn start time the hook reports
            return Seen::Running(0);
        }
        // Without busy, the screen can't tell whether it's running: don't override the hook's "running"
        if self.busy.is_empty() { Seen::Unsure } else { Seen::Quiet }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    fn tool(toml: &str) -> Tool {
        config::parse(&format!("[tools.toy]\n{toml}")).tool("toy").unwrap()
    }

    fn toy(toml: &str) -> Declared {
        Declared::new("toy", &tool(toml)).unwrap()
    }

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn it_starts_resumes_and_forks_from_the_templates() {
        let t = tool("resume = [\"--session\", \"{conv}\"]\nfork = [\"fork\", \"--from={conv}\"]\nargs = [\"-q\"]");
        let d = Declared::new("toy", &t).unwrap();
        let s = Session { args: vec!["--model".into(), "x".into()], ..Default::default() };
        let run = |how: How| d.argv(&t, &s, &how, "aca").unwrap().join(" ");
        assert_eq!(run(How::Fresh), "toy -q --model x");
        assert_eq!(run(How::Resume("c-1".into())), "toy --session c-1 -q --model x");
        assert_eq!(run(How::Fork("c-1".into())), "toy fork --from=c-1 -q --model x");
        assert!(d.can_fork());
        // Its own command: the template follows the command
        let wrapped = Tool { cmd: vec!["/w/run.sh".into()], ..t.clone() };
        assert_eq!(d.argv(&wrapped, &s, &How::Resume("c".into()), "aca").unwrap()[..3], ["/w/run.sh", "--session", "c"]);
    }

    #[test]
    fn it_only_resumes_when_it_knows_both_the_id_and_how() {
        let d = toy("resume = [\"--session\", \"{conv}\"]");
        assert_eq!(d.resume("c-1", &Run::default()), How::Resume("c-1".into()));
        assert_eq!(d.resume("", &Run::default()), How::Fresh);
        // Can't fork: even a fork can only start fresh
        assert_eq!(d.resume("", &Run { fork_from: "c-0".into(), ..Default::default() }), How::Fresh);
        let bare = toy("");
        assert_eq!(bare.resume("c-1", &Run::default()), How::Fresh);
        assert!(!bare.can_fork());
        let forky = toy("fork = [\"--fork\", \"{conv}\"]");
        assert_eq!(forky.resume("", &Run { fork_from: "c-0".into(), ..Default::default() }), How::Fork("c-0".into()));
    }

    #[test]
    fn the_screen_patterns_say_asking_or_busy() {
        let d = toy("busy = ['Working\\.\\.\\.']\nask = ['\\(y/n\\)']");
        assert_eq!(d.screen(&lines("> 改一下\n\n⠋ Working...\n")), Seen::Running(0));
        assert_eq!(d.screen(&lines("Working...\nRun rm -rf build? (y/n)\n")), Seen::Ask("Run rm -rf build? (y/n)".into()));
        assert_eq!(d.screen(&lines("done\n> ")), Seen::Quiet);
        // Long scrolled up doesn't count
        let old = format!("Working...\n{}", "x\n".repeat(20));
        assert_eq!(d.screen(&lines(&old)), Seen::Quiet);
        // Nothing set: nothing to see on the screen
        // No busy: the screen can't tell whether it's running, so it doesn't say "not running"
        assert_eq!(toy("").screen(&lines("Working...")), Seen::Unsure);
    }

    #[test]
    fn the_title_comes_from_the_hook_or_else_the_pane_when_asked() {
        let pane = toy("title = \"pane\"");
        assert!(pane.title_in_pane());
        assert_eq!(pane.title(&Run::default(), "⠙ 修登录 ").as_deref(), Some("修登录"));
        assert_eq!(pane.title(&Run::default(), "⠙ "), None);
        let said = Run { hook_title: "它报的".into(), ..Default::default() };
        assert_eq!(pane.title(&said, "⠙ 修登录").as_deref(), Some("它报的"));
        // Pane not chosen as the title source: the pane title is most likely the host name, don't take it
        assert_eq!(toy("").title(&Run::default(), "my-mac.local"), None);
    }

    #[test]
    fn mistakes_in_the_declaration_are_reported_with_the_tool_name() {
        let e = Declared::new("toy", &tool("busy = ['(']")).err().unwrap();
        assert!(e.contains("toy") && e.contains("busy"), "{e}");
        assert!(Declared::new("toy", &tool("title = \"transcript\"")).is_err());
    }
}
