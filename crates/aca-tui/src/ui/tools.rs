//! The tool menu (`n`, ctrl-e for the special environments): which tool and account a new session starts with, logins and
//! their quotas.

use super::*;

impl App {
    /// `n`: pick a tool first (accounts and tools named in config.toml; the special environments' behind `^E`), then fill the
    /// path like N
    pub(super) fn open_tools(&mut self) {
        match (self.tools_loader)() {
            Ok(v) if v.is_empty() => self.flash(t!("工具菜单为空", "the tool menu is empty")),
            Ok(v) => {
                self.load_tool_quotas();
                self.show_tools(v);
            }
            Err(e) => self.flash(&e),
        }
    }

    /// `^E` from the list: the special environments' rows on their own (see tools.rs)
    pub(super) fn open_special(&mut self) {
        let special = match (self.tools_loader)() {
            Ok(v) => crate::tools::split(v).1,
            Err(e) => return self.flash(&e),
        };
        if special.is_empty() {
            return self.flash(t!("没有特殊环境", "no special environments"));
        }
        self.load_tool_quotas();
        (self.tool_menu, self.tool_more, self.tool_special, self.tool_back) = (Some((special, 0)), vec![], true, None);
        self.ask_env_logins();
    }

    /// The menu's rows: this Mac's, the special environments' kept for `^E` (only those, when there's nothing else)
    pub(super) fn show_tools(&mut self, v: Vec<Tool>) {
        let (here, special) = crate::tools::split(v);
        self.tool_back = None;
        (self.tool_menu, self.tool_more, self.tool_special) = match here.is_empty() {
            true => (Some((special, 0)), vec![], true),
            false => (Some((here, 0)), special, false),
        };
        if self.tool_special {
            self.ask_env_logins();
        }
    }

    /// Close the menu, whichever rows it shows
    fn close_tools(&mut self) {
        (self.tool_menu, self.tool_back, self.tool_special, self.tool_relogin) = (None, None, false, None);
        self.tool_more.clear();
    }

    /// The special environments' accounts shown: whether each is logged in, asked in the background (as X does), the answers
    /// shown on their rows as they come
    fn ask_env_logins(&self) {
        let Some((rows, _)) = &self.tool_menu else { return };
        self.tool_logins.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let accounts =
            rows.iter().filter(|t| matches!(t.agent, Agent::Claude | Agent::Codex) && t.problem.is_none() && !t.account.is_empty());
        for t in accounts {
            let (logins, ask, agent, account) = (self.tool_logins.clone(), self.rehome_ask, t.agent, t.account.clone());
            let spec = crate::tools::spec(&t.tool, &t.account);
            let go = move || {
                let got = ask(agent, &account);
                logins.lock().unwrap_or_else(|e| e.into_inner()).insert(spec, got);
            };
            if self.rehome_background {
                std::thread::spawn(go);
            } else {
                go();
            }
        }
    }

    /// `l` among the special environments': log the highlighted account in, in its environment. A logged-in Codex asks first
    fn ask_login(&mut self, i: usize) {
        let Some(t) = self.tool_menu.as_ref().and_then(|(m, _)| m.get(i)).cloned() else { return };
        if !matches!(t.agent, Agent::Claude | Agent::Codex) || t.account.is_empty() {
            return self.flash(t!("终端不用登录", "a terminal has no login"));
        }
        if let Some(p) = &t.problem {
            return self.flash(p);
        }
        let spec = crate::tools::spec(&t.tool, &t.account);
        let logged = matches!(self.tool_logins.lock().unwrap_or_else(|e| e.into_inner()).get(&spec), Some(crate::accounts::Login::In(_)));
        if t.agent == Agent::Codex && logged {
            self.tool_relogin = Some(i);
            return;
        }
        self.start_login(&t);
    }

    /// A terminal at the bottom running the account's own login in its environment (`aca run`: inside a cage, with unis's
    /// variables on this Mac); its link pops up as on the accounts page, with 「在别的电脑登录」 (log in on another computer) when
    /// the login waits here for the browser to come back
    fn start_login(&mut self, t: &Tool) {
        let remote = aca_core::agent::remote(&aca_core::config::load());
        let home = std::env::var("HOME").unwrap_or_default();
        let run = crate::accounts::relogin_cmd(t.agent, &t.account);
        self.close_tools();
        self.term_req = Some(crate::acctpage::login_term(&self.login_links, &home, remote, &t.account, t.agent, run));
    }

    /// The quota after each account in the tool menu: the same copy X's box reads
    pub(super) fn load_tool_quotas(&mut self) {
        self.tool_quotas = [Agent::Claude, Agent::Codex].into_iter().flat_map(self.rehome_quotas).collect();
    }

    pub(super) fn on_tool_key(&mut self, k: KeyEvent) {
        // Asked whether to log a logged-in Codex in again: only y does
        if let Some(i) = self.tool_relogin.take() {
            if k.code == KeyCode::Char('y')
                && let Some(t) = self.tool_menu.as_ref().and_then(|(m, _)| m.get(i)).cloned()
            {
                self.start_login(&t);
            }
            return;
        }
        let Some((menu, sel)) = self.tool_menu.as_mut() else { return };
        let n = menu.len();
        match k.code {
            KeyCode::Char('l') if self.tool_special && self.handoff.is_none() => {
                let i = *sel;
                self.ask_login(i);
            }
            KeyCode::Down | KeyCode::Char('j') => *sel = (*sel + 1) % n,
            KeyCode::Up | KeyCode::Char('k') => *sel = (*sel + n - 1) % n,
            KeyCode::Enter => {
                let i = *sel;
                self.pick_tool(i);
            }
            KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < n => self.pick_tool(c as usize - '1' as usize),
            // Adding accounts is on the accounts page (pick Claude Code or Codex first);
            // after adding it stays on that page, showing whether it's logged in
            KeyCode::Char('+') if self.handoff.is_none() && !self.tool_special => {
                if let Some(page) = self.accounts_page {
                    self.close_tools();
                    self.open(page(true));
                }
            }
            // The special environments' rows, switched to from n's (a hand off in progress carries on there)
            KeyCode::Char('e') if k.modifiers.contains(KeyModifiers::CONTROL) && !self.tool_more.is_empty() => {
                let more = std::mem::take(&mut self.tool_more);
                self.tool_back = self.tool_menu.replace((more, 0));
                self.tool_special = true;
                self.ask_env_logins();
            }
            // Switched to: back to n's, ^E still there
            KeyCode::Esc | KeyCode::Char('q') if self.tool_back.is_some() => {
                self.tool_more = self.tool_menu.take().map(|(m, _)| m).unwrap_or_default();
                self.tool_menu = self.tool_back.take();
                self.tool_special = false;
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.close_tools();
                self.handoff = None;
            }
            _ => {}
        }
    }

    fn pick_tool(&mut self, i: usize) {
        // A row that can't start (account not registered, broken tool) can't be picked: the menu stays, the bottom bar says why
        if let Some(p) = self.tool_menu.as_ref().and_then(|(m, _)| m[i].problem.clone()) {
            return self.flash(&p);
        }
        let Some((menu, _)) = self.tool_menu.take() else { return };
        self.close_tools();
        let t = &menu[i];
        if let Some(p) = self.handoff.take() {
            let mut d = p.draft;
            (d.tool, d.account) = (t.tool.clone(), t.account.clone());
            d.label = t.name.clone();
            match d.finish() {
                Ok(req) => self.job = Some(Job::Handoff { t: p.t, from: p.from, req, to: t.agent, note: p.note }),
                Err(e) => self.flash(&e),
            }
            return;
        }
        match self.draft() {
            Ok(mut d) => {
                (d.tool, d.account) = (t.tool.clone(), t.account.clone());
                d.label = t.name.clone();
                self.new_draft = Some(d);
                self.pick = self.pick_first();
            }
            Err(e) => self.flash(&e),
        }
    }
}

/// The tool menu shows at most this many rows at a time; more scroll with the highlight
const TOOL_ROWS: usize = 12;

/// The login column among the special environments' accounts: as X shows it, fixed width
fn login_cell(l: Option<&crate::accounts::Login>) -> Span<'static> {
    use crate::accounts::Login;
    let (text, st) = match l {
        None => ("…", dim()),
        Some(Login::In(_)) => (t!("● 已登录", "● logged in"), Style::new().fg(theme::pal().green)),
        Some(Login::Out) => (t!("○ 未登录", "○ logged out"), theme::warn().add_modifier(Modifier::BOLD)),
        Some(Login::Unknown(_)) => (t!("? 未知", "? unknown"), dim()),
    };
    Span::styled(format!("{text}{}  ", " ".repeat(14usize.saturating_sub(text.width()))), st)
}

/// `n`'s tool menu: one tool per row, "number name provider quota" (tools you named show the command), name colored by provider, the
/// highlighted row on light purple. `n` / `H`'s tool menu; with pass = picking for a hand off: the title says who's handed off, and
/// mentions if it's running or has a note. The special environments' (`^E`) in their own color, each environment's rows under a
/// heading saying what it is, each account whether it's logged in (`logins`, by `tool:account`)
pub(super) fn draw_tools(
    f: &mut Frame,
    menu: &[Tool],
    sel: usize,
    pass: Option<&Pass>,
    quotas: &[aca_records::usage::Quota],
    logins: &HashMap<String, crate::accounts::Login>,
    set: Set,
) {
    let (at, off) = (epoch() as i64, aca_records::usage::local_offset());
    let special = matches!(set, Set::Special { .. });
    let color = if special { theme::pal().amber } else { theme::pal().accent };
    modal(f, 72, color, |w| {
        let name_w = menu.iter().map(|t| t.name.width()).max().unwrap_or(0).min(12);
        let start = (sel + 1).saturating_sub(TOOL_ROWS).min(menu.len().saturating_sub(TOOL_ROWS));
        let mut title =
            pass.map_or_else(|| t!("选择工具", "Pick a tool").to_string(), |p| tr!("交接「{}」给", "Hand off “{}” to", p.t.title));
        if special {
            title = match pass {
                None => t!("特殊环境", "Special environments").to_string(),
                Some(_) => format!("{title}{}", t!(" · 特殊环境", " · special environments")),
            };
        }
        let esc = if matches!(set, Set::Special { back: true, .. }) { t!("返回", "back") } else { t!("取消", "cancel") };
        let mut out = vec![modal_title(&title, Style::new().fg(color), esc_hint(esc), w), Line::raw("")];
        if let Some(p) = pass {
            let mut marks = vec![];
            if p.busy {
                marks.push(Span::styled(t!("仍在运行", "it's still running"), theme::warn()));
            }
            if p.note.is_some() {
                if !marks.is_empty() {
                    marks.push(Span::styled(" · ", theme::faint()));
                }
                marks.push(Span::styled(t!("带上便签", "with the note"), dim()));
            }
            if !marks.is_empty() {
                out.extend([Line::from(marks), Line::raw("")]);
            }
        }
        for (i, t) in menu.iter().enumerate().skip(start).take(TOOL_ROWS) {
            // An environment's first row (or the first one shown): its heading
            if special && (i == start || menu[i - 1].within != t.within) {
                if i > start {
                    out.push(Line::raw(""));
                }
                let head = Span::styled(t.within.clone(), Style::new().fg(color).add_modifier(Modifier::BOLD));
                let under = clip(&t.under, w.saturating_sub(t.within.width() + 2));
                out.push(Line::from(vec![head, Span::raw("  "), Span::styled(under, dim())]));
            }
            let num = if i < 9 { format!("{} ", i + 1) } else { "  ".into() };
            let name = clip(&t.name, name_w);
            let mut line = vec![Span::styled(num, dim()), Span::raw(""), Span::raw("  ")];
            let room = w.saturating_sub(2 + name_w + 2);
            match &t.problem {
                // Unusable: name not colored, the reason after it
                Some(p) => {
                    line[1] = Span::styled(name, dim());
                    line.push(Span::styled(clip(p, room).trim_end().to_string(), theme::warn()));
                }
                // Account: provider, quota (only Claude and Codex have quota)
                None if matches!(t.agent, Agent::Claude | Agent::Codex) && t.note.is_empty() => {
                    line[1] = Span::styled(name, agent_style(t.agent).add_modifier(Modifier::BOLD));
                    let fam = if t.agent == Agent::Codex { "Codex " } else { "Claude" };
                    line.push(Span::styled(format!("{fam}  "), dim()));
                    if special {
                        line.push(login_cell(logins.get(&crate::tools::spec(&t.tool, &t.account))));
                    }
                    let who = if t.account.is_empty() { &t.name } else { &t.account };
                    line.extend(crate::rehome::quota_spans(crate::rehome::quota_of(quotas, t.agent, who).as_ref(), at, off));
                }
                None if matches!(t.agent, Agent::Pi | Agent::Opencode) && t.note.is_empty() => {
                    line[1] = Span::styled(name, agent_style(t.agent).add_modifier(Modifier::BOLD));
                    line.push(Span::styled(if t.agent == Agent::Pi { "Pi" } else { "OpenCode" }, dim()));
                }
                None => {
                    line[1] = Span::styled(name, agent_style(t.agent).add_modifier(Modifier::BOLD));
                    line.push(Span::styled(clip(&t.note, room).trim_end().to_string(), dim()));
                }
            }
            let mut line = Line::from(line);
            if i == sel {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(theme::pal().sel));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        // Logging a logged-in Codex in again: what it costs, then only y goes ahead
        if let Set::Special { relogin: Some(i), .. } = set {
            let name = menu.get(i).map_or("", |t| t.name.as_str());
            let said = tr!(
                "{name} 现在已登录。Codex 开始登录时会先清掉它，中途退出就是未登录",
                "{name} is logged in now. Codex clears it as the login starts: if you don't finish, it's left logged out"
            );
            out.extend(crate::setup::fold(&said, w).into_iter().map(|l| Line::styled(l, theme::warn())));
            let keys = [("y", t!("重新登录", "log in again")), (t!("其他键", "any other key"), t!("取消", "leave it"))];
            out.push(keys_line(&keys, vec![], w));
            return out;
        }
        // No adding accounts during a hand off (pressing + would drop the hand off halfway), nor among the special environments'
        // (their accounts are bound in config.toml); logging in there, not during a hand off either
        let mut keys: Vec<(&str, &str)> = match (pass.is_some(), special) {
            (true, _) => vec![("⏎", t!("交接", "hand off"))],
            (false, true) => vec![("⏎", t!("选择", "select")), ("l", t!("登录", "log in"))],
            (false, false) => vec![("⏎", t!("选择", "select")), ("+", t!("添加账号", "add account"))],
        };
        if set == Set::Here(true) {
            keys.push(("^E", t!("特殊环境", "special environments")));
        }
        out.push(keys_line(&keys, vec![], w));
        out
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{
        ctrl, fake_source, fixture, handoff_tools, hints, pin, press, reader_rows, sandbox_tools, select, sessions, shows,
    };

    #[test]
    fn ctrl_e_shows_the_special_environments_apart_each_under_its_heading() {
        let mut app = fixture().with_tools(sandbox_tools);
        pin(&mut app, "跑");
        pin(&mut app, "跑"); // select 「跑」
        // n: only this Mac's, with ^E there
        press(&mut app, KeyCode::Char('n'));
        let rows = reader_rows(&mut app, 72, 16);
        shows(&rows, &["选择工具", "Esc 取消", "1 shell  ~/.shell", "2 sub    Claude", "^E 特殊环境"]);
        assert!(!rows.iter().any(|r| r.contains("team")), "{rows:#?}");
        // ^E switches: each environment under a heading saying what it is, no adding accounts; Esc goes back to n's
        ctrl(&mut app, 'e');
        let rows = reader_rows(&mut app, 72, 20);
        shows(
            &rows,
            &[
                "特殊环境",
                "Esc 返回",
                "unis  本机 · Los_Angeles · 代理 127.0.0.1:28888",
                "1 shell:unis  终端",
                "2 team-unis   Codex",
                "work  ssh work",
                "3 shell:work  终端",
                "4 team        Claude",
                "5 team        Codex",
            ],
        );
        assert!(!rows.iter().any(|r| r.contains("添加账号")), "{rows:#?}");
        press(&mut app, KeyCode::Esc);
        assert!(!app.tool_special && app.tool_menu.as_ref().is_some_and(|(m, _)| m.len() == 2) && app.tool_more.len() == 5);
        press(&mut app, KeyCode::Esc);
        assert!(app.tool_menu.is_none() && app.new_draft.is_none() && app.tool_more.is_empty());
        // ^E from the list: those alone, Esc closes; picking one, the path next as from any row
        ctrl(&mut app, 'e');
        shows(&reader_rows(&mut app, 72, 20), &["特殊环境", "Esc 取消"]);
        press(&mut app, KeyCode::Char('2'));
        assert!(app.tool_menu.is_none() && !app.tool_special);
        let d = app.new_draft.as_ref().unwrap();
        assert_eq!((d.label.as_str(), d.tool.as_str(), d.account.as_str()), ("team-unis", "codex", "team-unis"));
        // None configured: says so
        let mut none = fixture().with_tools(handoff_tools);
        ctrl(&mut none, 'e');
        assert!(none.tool_menu.is_none() && none.msg.as_ref().is_some_and(|m| m.0 == "没有特殊环境"));

        // H too, without the terminals; the hand off carries on there
        let mut app = fixture().with_tools(sandbox_tools).with_handoff(fake_source).with_group_hints(hints);
        app.set_sessions(sessions());
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('H'));
        shows(&reader_rows(&mut app, 72, 14), &["1 sub  Claude", "^E 特殊环境"]);
        ctrl(&mut app, 'e');
        shows(&reader_rows(&mut app, 72, 18), &["交接「跑」给 · 特殊环境", "1 team-unis  Codex"]);
        press(&mut app, KeyCode::Char('1'));
        let Some(Job::Handoff { req, to, .. }) = app.job.clone() else { panic!("{:?}", app.job) };
        assert_eq!((req.spec().as_str(), to), ("codex:team-unis", Agent::Codex));
    }
}
