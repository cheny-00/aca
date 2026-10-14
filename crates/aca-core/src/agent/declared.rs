//! 声明式工具:不写代码,在 config.toml 里写一段 `[tools.<名字>]` 就能接进来(照 agent-deck 的 `[tools.X]`)。
//!
//! ```toml
//! [tools.aider]
//! cmd = ["aider"]                     # 不写就是工具名
//! home_var = "AIDER_HOME"             # 账号的 home(配置目录)交给哪个环境变量;不写的话账号里只能用 env / args
//! resume = ["--session", "{conv}"]    # 接着对话:{conv} 换成对话 id
//! fork = ["--fork", "{conv}"]         # 从一段对话分叉;不写就不能分叉
//! busy = ['Working\.\.\.']            # 屏幕底部出现就算在跑(正则,任一条)
//! ask = ['\(y/n\)', 'Allow .*\?']     # 屏幕底部出现就算在等你
//! title = "pane"                      # 标题取窗格标题(开头转圈的字符去掉);不写就只用 hook 报的
//! env = { FOO = "1" }                 # 起的时候加的环境变量
//! ```
//!
//! 只写这些:能起、停、在原地重启,列表里看得出在跑 / 在等你。对话 id 要有人报上来才接得上对话:
//! agent 自己有插件 / 扩展机制的,写一个小转发,事件来了跑 `"$ACA_BIN" hook "$ACA_TOOL"`,
//! 标准输入给一个 hook 协议的 JSON(形状见 [`super::lifecycle`]),对话 id、最后一条回复、跑完没看、标题就都有了。

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
            Regex::new(p).map_err(|e| tr!("工具 {tool} 的 {field} 里 {p} 不是正则:{e}", "{p} in {field} of tool {tool} isn't a regex: {e}"))
        })
        .collect()
}

/// `["--session", "{conv}"]` → `["--session", "c-1"]`
fn fill(template: &[String], conv: &str) -> Vec<String> {
    template.iter().map(|a| a.replace("{conv}", conv)).collect()
}

/// 窗格标题开头转圈 / 状态的字符去掉:盲文点阵那一段(⠋⠙…)和 Claude 那几个星号
fn strip_spinner(title: &str) -> &str {
    title.trim_start_matches(|c: char| c.is_whitespace() || ('\u{2800}'..='\u{28FF}').contains(&c) || "✳✶✻✽✢·*".contains(c)).trim_end()
}

impl Declared {
    pub fn new(name: &str, t: &Tool) -> Res<Declared> {
        if !matches!(t.title.as_str(), "" | "pane") {
            return Err(tr!("工具 {name} 的 title 只能写 \"pane\" 或者不写", "tool {name}: title can only be \"pane\" or left out"));
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

    /// 知道对话 id、也写了怎么接,就接;分叉出来还没报过 id 的,从原来那段重新分
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

    /// hook 报了标题就用它;没报、又说了看窗格标题,就取窗格标题
    fn title(&self, r: &Run, pane_title: &str) -> Option<String> {
        if !r.hook_title.is_empty() {
            return Some(r.hook_title.clone());
        }
        let t = strip_spinner(pane_title);
        (self.pane_title && !t.is_empty()).then(|| t.to_string())
    }

    /// 只看底部这些行:往上翻的历史里出现过的字不算
    fn screen(&self, lines: &[String]) -> Seen {
        let nonempty: Vec<&str> = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
        let tail = &nonempty[nonempty.len().saturating_sub(15)..];
        if let Some(l) = tail.iter().rev().find(|l| self.ask.iter().any(|re| re.is_match(l))) {
            return Seen::Ask(clip(l, 200));
        }
        if tail.iter().any(|l| self.busy.iter().any(|re| re.is_match(l))) {
            // 跑了多久屏幕上不一定有:列表用 hook 报的那一轮开始的时刻
            return Seen::Running(0);
        }
        // 没写 busy 的,屏幕上看不出在不在跑:别把 hook 报的「在跑」盖掉
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
        // 自己定了命令:模板接在命令后面
        let wrapped = Tool { cmd: vec!["/w/run.sh".into()], ..t.clone() };
        assert_eq!(d.argv(&wrapped, &s, &How::Resume("c".into()), "aca").unwrap()[..3], ["/w/run.sh", "--session", "c"]);
    }

    #[test]
    fn it_only_resumes_when_it_knows_both_the_id_and_how() {
        let d = toy("resume = [\"--session\", \"{conv}\"]");
        assert_eq!(d.resume("c-1", &Run::default()), How::Resume("c-1".into()));
        assert_eq!(d.resume("", &Run::default()), How::Fresh);
        // 不能分叉的,就算是分叉出来的也只能新开
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
        // 早就滚上去的不算
        let old = format!("Working...\n{}", "x\n".repeat(20));
        assert_eq!(d.screen(&lines(&old)), Seen::Quiet);
        // 什么都没写的:屏幕上看不出东西
        // 没写 busy:屏幕上看不出在不在跑,不说「没在跑」
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
        // 没说看窗格:窗格标题多半是主机名,不拿
        assert_eq!(toy("").title(&Run::default(), "my-mac.local"), None);
    }

    #[test]
    fn mistakes_in_the_declaration_are_reported_with_the_tool_name() {
        let e = Declared::new("toy", &tool("busy = ['(']")).err().unwrap();
        assert!(e.contains("toy") && e.contains("busy"), "{e}");
        assert!(Declared::new("toy", &tool("title = \"transcript\"")).is_err());
    }
}
