//! The tool menu (`n`, ctrl-e for the special environments): which tool and account a new session starts with, each account's
//! login and quota (i logs in, a opens the accounts page on it).

use super::*;
use crate::accounts::Login;

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
        self.ask_tool_logins();
    }

    /// The menu's rows: this Mac's, the special environments' kept for `^E` (only those, when there's nothing else)
    pub(super) fn show_tools(&mut self, v: Vec<Tool>) {
        let (here, special) = crate::tools::split(v);
        self.tool_back = None;
        (self.tool_menu, self.tool_more, self.tool_special) = match here.is_empty() {
            true => (Some((special, 0)), vec![], true),
            false => (Some((here, 0)), special, false),
        };
        self.ask_tool_logins();
    }

    /// Close the menu, whichever rows it shows
    fn close_tools(&mut self) {
        (self.tool_menu, self.tool_back, self.tool_special, self.tool_relogin) = (None, None, false, None);
        self.tool_more.clear();
    }

    /// The accounts shown: whether each is logged in, asked in the background (as X does), the answers shown on their rows as
    /// they come. Answers are kept for `LOGINS_KEPT` (each ask is a process per account), and dropped when a login starts here
    fn ask_tool_logins(&mut self) {
        let Some((rows, _)) = &self.tool_menu else { return };
        let fresh = self.tool_logins_at.is_some_and(|t| t.elapsed() < LOGINS_KEPT);
        if !fresh {
            self.tool_logins.lock().unwrap_or_else(|e| e.into_inner()).clear();
            self.tool_logins_at = Some(Instant::now());
        }
        let known = self.tool_logins.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let accounts = rows.iter().filter(|t| {
            t.has_login() && t.problem.is_none() && (t.google_login() || !known.contains_key(&crate::tools::spec(&t.tool, &t.account)))
        });
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

    /// Where the accounts page opens when asked for over the list (^S then a): on the highlighted session's account (a group, a
    /// rail row, or a session that isn't Claude / Codex: on the first one)
    pub(super) fn accounts_opening(&self) -> crate::acctpage::Opening {
        let s = self.current().filter(|_| !self.on_group_row() && !self.on_rail());
        if s.is_some_and(|s| s.agent == Agent::Agy && s.account.is_empty()) {
            return crate::acctpage::Opening::At(Agent::Agy, String::new());
        }
        let s = s.filter(|s| matches!(s.agent, Agent::Claude | Agent::Codex));
        match s.map(|s| (s.agent, if s.account.is_empty() { s.who.clone() } else { s.account.clone() })) {
            Some((a, name)) if !name.is_empty() => crate::acctpage::Opening::At(a, name),
            _ => crate::acctpage::Opening::First,
        }
    }

    /// What the highlighted account's login is now (None: not asked yet, or not an account)
    fn tool_login(&self, t: &Tool) -> Option<crate::accounts::Login> {
        self.tool_logins.lock().unwrap_or_else(|e| e.into_inner()).get(&crate::tools::spec(&t.tool, &t.account)).cloned()
    }

    /// i: log the highlighted account in, in its environment, as on the accounts page; a logged-in one (or one not asked about
    /// yet) asks first. During a hand off the menu closes with it (H again once logged in)
    fn ask_login(&mut self, i: usize) {
        let Some(t) = self.tool_menu.as_ref().and_then(|(m, _)| m.get(i)).cloned() else { return };
        if t.google_login() && t.problem.is_none() {
            match crate::acctpage::agy_login_term(&self.login_links, &aca_core::config::load()) {
                Ok(req) => {
                    self.close_tools();
                    self.handoff = None;
                    self.term_req = Some(req);
                }
                Err(e) => self.flash(&e),
            }
            return;
        }
        if t.agent == Agent::Agy {
            return self.accounts_at(i);
        }
        if !matches!(t.agent, Agent::Claude | Agent::Codex) {
            return self.flash(t!("终端不用登录", "a terminal has no login"));
        }
        if t.account.is_empty() {
            return self.flash(&tr!(
                "{} 用的是工具自己的目录,在账号页登录",
                "{} uses the tool's own directory: log in on the accounts page",
                t.name
            ));
        }
        if let Some(p) = &t.problem {
            return self.flash(p);
        }
        match self.tool_login(&t) {
            Some(Login::Unsafe(why)) => self.flash(&why),
            Some(Login::Out) => self.start_login(&t),
            _ => self.tool_relogin = Some(i),
        }
    }

    /// a: the accounts page, on the highlighted account (a terminal or a tool's own directory: on the first). The menu closes, a
    /// hand off with it
    fn accounts_at(&mut self, i: usize) {
        let Some(page) = self.accounts_page else { return };
        let at = self.tool_menu.as_ref().and_then(|(m, _)| m.get(i));
        let o = match at {
            Some(t) if t.agent == Agent::Agy => crate::acctpage::Opening::At(t.agent, String::new()),
            Some(t) if matches!(t.agent, Agent::Claude | Agent::Codex) && !t.account.is_empty() => {
                crate::acctpage::Opening::At(t.agent, t.account.clone())
            }
            _ => crate::acctpage::Opening::First,
        };
        self.close_tools();
        self.handoff = None;
        self.open(page(o));
    }

    /// A terminal at the bottom running the account's own login in its environment (`aca run`: inside a cage, with unis's
    /// variables on this Mac); its link pops up as on the accounts page, with 「在别的电脑登录」 (log in on another computer) when
    /// the login waits here for the browser to come back
    fn start_login(&mut self, t: &Tool) {
        // Asked a moment ago, but not answered yet when y was pressed: look at the directory once more before writing into it
        if let Some(why) = crate::accounts::unsafe_dir(t.agent, &t.account) {
            return self.flash(&why);
        }
        let remote = aca_core::agent::remote(&aca_core::config::load());
        let home = std::env::var("HOME").unwrap_or_default();
        let run = crate::accounts::relogin_cmd(t.agent, &t.account);
        // Logging in changes the answer: ask everything again next time the menu opens
        self.tool_logins_at = None;
        self.close_tools();
        self.handoff = None;
        self.term_req = Some(crate::acctpage::login_term(&self.login_links, &home, remote, &t.account, t.agent, run));
    }

    /// The quota after each account in the tool menu: the same copy X's box reads
    pub(super) fn load_tool_quotas(&mut self) {
        self.tool_quotas = [Agent::Claude, Agent::Codex, Agent::Agy].into_iter().flat_map(self.rehome_quotas).collect();
    }

    pub(super) fn on_tool_key(&mut self, k: KeyEvent) {
        // Asked whether to log a logged-in account in again: only y does
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
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if k.code == KeyCode::Char('?') && !ctrl {
            let set = match self.tool_special {
                true => Set::Special { back: self.tool_back.is_some() },
                false => Set::Here(!self.tool_more.is_empty()),
            };
            let logins = self.tool_logins.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let keys = tool_keys(menu.get(*sel), self.handoff.is_some(), &logins, set);
            return self.open_sheet(&keys);
        }
        // The key that opened it closes it: n (new), H (hand off), ^E (the special environments; switched to from n's, back there)
        let opener = match k.code {
            KeyCode::Char('n') => !ctrl && self.handoff.is_none() && !self.tool_special,
            KeyCode::Char('H') => self.handoff.is_some(),
            KeyCode::Char('e') => ctrl && self.tool_special,
            _ => false,
        };
        if opener && self.tool_back.is_some() && k.code != KeyCode::Char('H') {
            self.tool_more = self.tool_menu.take().map(|(m, _)| m).unwrap_or_default();
            self.tool_menu = self.tool_back.take();
            self.tool_special = false;
            return;
        }
        if opener {
            self.close_tools();
            self.handoff = None;
            return;
        }
        match k.code {
            KeyCode::Char('i') => {
                let i = *sel;
                self.ask_login(i);
            }
            KeyCode::Char('a') => {
                let i = *sel;
                self.accounts_at(i);
            }
            KeyCode::Up | KeyCode::Down if k.modifiers.contains(KeyModifiers::SHIFT) && !self.tool_special && self.handoff.is_none() => {
                self.move_tool(k.code == KeyCode::Up, &crate::config::path(), crate::accounts::on_path);
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
                    self.open(page(crate::acctpage::Opening::Adding));
                }
            }
            // The special environments' rows, switched to from n's (a hand off in progress carries on there)
            KeyCode::Char('e') if k.modifiers.contains(KeyModifiers::CONTROL) && !self.tool_more.is_empty() => {
                let more = std::mem::take(&mut self.tool_more);
                self.tool_back = self.tool_menu.replace((more, 0));
                self.tool_special = true;
                self.ask_tool_logins();
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

    fn move_tool(&mut self, up: bool, path: &std::path::Path, installed: fn(&str) -> bool) {
        let Some((menu, sel)) = self.tool_menu.as_mut() else { return };
        let to = if up { sel.checked_sub(1) } else { Some(*sel + 1).filter(|&i| i < menu.len()) };
        let Some(to) = to else { return };
        match crate::accounts::move_next_to(path, &menu[*sel].name, &menu[to].name, up, installed) {
            Ok(true) => {
                menu.swap(*sel, to);
                *sel = to;
            }
            Ok(false) => {}
            Err(e) => self.flash(&e),
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

/// How long the menus keep each account's login answer before asking again
const LOGINS_KEPT: Duration = Duration::from_secs(300);

/// `n`'s tool menu: one tool per row, number then the account as everywhere accounts are listed (name, tool, login, quota; tools
/// you named show the command), name colored by provider, the highlighted row on light purple. `n` / `H`'s tool menu; with pass =
/// picking for a hand off: the title says who's handed off, and mentions if it's running or has a note. The special environments'
/// (`^E`) in their own color, each environment's rows under a heading saying what it is. `logins` by `tool:account`; relogin = the
/// row asked whether to log in again
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_tools(
    f: &mut Frame,
    menu: &[Tool],
    sel: usize,
    pass: Option<&Pass>,
    quotas: &[aca_records::usage::Quota],
    logins: &HashMap<String, crate::accounts::Login>,
    set: Set,
    relogin: Option<usize>,
) {
    let (at, off) = (epoch() as i64, aca_records::usage::local_offset());
    // The account rows are one table: each quota window in its own column (scrolling doesn't move them)
    let account = |t: &Tool| t.note.is_empty() && (matches!(t.agent, Agent::Claude | Agent::Codex) || t.google_login());
    let accts = || menu.iter().filter(|t| t.problem.is_none() && account(t));
    let tool_w = accts().map(|t| aca_records::words::agent_label(t.agent, "").width()).max().unwrap_or(0).max(6) + 2;
    let quota = |t: &Tool| crate::rehome::quota_of(quotas, t.agent, if t.account.is_empty() { &t.name } else { &t.account });
    let qs: Vec<_> = accts().filter_map(quota).collect();
    let cols = crate::rehome::Cols::of(&qs, at).logins(accts().any(Tool::has_login));
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
            // Each on its own line: what running on means (as X says it, wrapped on a phone), then that the note goes along
            let mut said = vec![];
            if p.busy {
                let warn = t!("⚠ 原会话仍在运行,两边会同时修改这个目录", "⚠ still running: both would change this directory at once");
                said.extend(crate::kit::wrap_width(warn, w).into_iter().map(|l| Line::styled(l, theme::warn())));
            }
            if p.note.is_some() {
                said.push(Line::styled(t!("带上便签", "with the draft"), dim()));
            }
            if !said.is_empty() {
                out.extend(said);
                out.push(Line::raw(""));
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
            let mut below = vec![];
            match &t.problem {
                // Unusable: name not colored, the reason after it
                Some(p) => {
                    line[1] = Span::styled(name, dim());
                    line.push(Span::styled(clip(p, room).trim_end().to_string(), theme::warn()));
                }
                // Account: tool, login (the tool's own directory has none to ask about here), quota
                None if account(t) => {
                    line[1] = Span::styled(name, agent_style(t.agent).add_modifier(Modifier::BOLD));
                    line.push(Span::styled(clip(&aca_records::words::agent_label(t.agent, ""), tool_w), dim()));
                    let login = t.has_login().then(|| logins.get(&crate::tools::spec(&t.tool, &t.account)));
                    let q = quota(t);
                    let room = room.saturating_sub(tool_w);
                    let tail = crate::rehome::account_row(
                        &cols,
                        None,
                        login,
                        q.as_ref(),
                        (at, off),
                        if login.is_none() || t.google_login() { usize::MAX } else { room },
                    );
                    if let Some(q) =
                        q.filter(|_| (login.is_none() || t.google_login()) && tail.iter().map(Span::width).sum::<usize>() > room)
                    {
                        if let Some(l) = login {
                            line.push(crate::rehome::login_cell(l));
                        }
                        for mut row in crate::rehome::wrap_quota(crate::rehome::quota_spans(Some(&q), at, off), w.saturating_sub(2)) {
                            row.spans.insert(0, Span::raw("  "));
                            below.push(row);
                        }
                    } else {
                        line.extend(tail);
                    }
                }
                None if matches!(t.agent, Agent::Pi | Agent::Opencode | Agent::Agy) && t.note.is_empty() => {
                    line[1] = Span::styled(name, agent_style(t.agent).add_modifier(Modifier::BOLD));
                    line.push(Span::styled(aca_records::words::agent_label(t.agent, ""), dim()));
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
            out.extend(below);
        }
        out.push(Line::raw(""));
        // Logging a logged-in account in again: what it costs, then only y goes ahead (as on the accounts page)
        if let Some(t) = relogin.and_then(|i| menu.get(i)) {
            let login = logins.get(&crate::tools::spec(&t.tool, &t.account));
            out.extend(crate::acctpage::relogin_lines(t.agent, &t.account, login, w));
            return out;
        }
        out.push(keys_line_help(&crate::sheet::foot(&tool_keys(menu.get(sel), pass.is_some(), logins, set)), vec![], w));
        out
    });
}

/// The menu's keys, for its bottom bar and `?`. No adding accounts during a hand off (pressing + would drop the hand off
/// halfway), nor among the special environments' (their accounts are bound in config.toml). The bottom bar writes i only when
/// the highlighted one isn't logged in (it works on any account, asking first when it is)
fn tool_keys(cur: Option<&Tool>, pass: bool, logins: &HashMap<String, Login>, set: Set) -> Vec<crate::sheet::Key> {
    use crate::sheet::{key, more};
    let special = matches!(set, Set::Special { .. });
    let mut keys = vec![key("⏎", if pass { t!("交接", "hand off") } else { t!("选择", "select") })];
    let out = cur.is_some_and(|t| t.has_login() && logins.get(&crate::tools::spec(&t.tool, &t.account)) == Some(&Login::Out));
    keys.push(if out { key("i", t!("登录", "log in")) } else { more("i", t!("登录", "log in")) });
    keys.push(key("a", t!("账号页", "accounts")));
    if !pass && !special {
        keys.insert(0, key("⇧↑↓", t!("移动", "move")));
        keys.push(key("+", t!("添加账号", "add account")));
    }
    if set == Set::Here(true) {
        keys.push(key("^E", t!("特殊环境", "special environments")));
    }
    let esc = if matches!(set, Set::Special { back: true }) { t!("返回", "back") } else { t!("取消", "cancel") };
    keys.extend([more("1-9", t!("选这一个", "this one")), more("↑↓", t!("选择", "select")), more("Esc", esc)]);
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agy_native_menu_snapshots_and_saved_order() {
        use crate::acctpage::tests::{MENU_CONFIG, installed, login, quotas};
        fn path() -> std::path::PathBuf {
            std::env::temp_dir().join(format!("aca-native-menu-{}", std::process::id())).join("config.toml")
        }
        fn load() -> Result<Vec<Tool>, String> {
            let text = std::fs::read_to_string(path()).unwrap();
            let lay = crate::tools::Layout {
                menu: crate::config::list_in(&text, "ui", "menu"),
                hide: crate::config::list_in(&text, "ui", "hide"),
            };
            Ok(crate::tools::menu_with(&aca_core::config::parse(&text), &lay, installed))
        }
        std::fs::create_dir_all(path().parent().unwrap()).unwrap();
        std::fs::write(path(), MENU_CONFIG).unwrap();
        let mut app = fixture().with_tools(load).with_rehome(quotas, login, false);
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Up);
        for w in [110, 40] {
            let rows = reader_rows(&mut app, w, 24);
            let start = rows.iter().position(|r| r.contains("选择工具")).unwrap();
            shows(&rows[start..], &["8 agy", "Antigravity", "5h 2% · 7d", "0%", "⇧↑↓ 移动"]);
            assert!(rows.iter().find(|r| r.contains("8 agy")).unwrap().contains('✓'));
            println!("n {w}\n{}", rows[start..].join("\n"));
        }
        for i in (0..7).rev() {
            app.move_tool(true, &path(), installed);
            let (m, sel) = app.tool_menu.as_ref().unwrap();
            assert_eq!((*sel, m[*sel].name.as_str()), (i, "agy"));
        }
        let saved = std::fs::read_to_string(path()).unwrap();
        assert_eq!(
            saved,
            MENU_CONFIG.replace(
                r#"["sub", "ding", "codex", "acme", "zeta", "official", "toys"]"#,
                r#"["agy", "shell", "sub", "ding", "codex", "acme", "zeta", "official", "toys", "pi"]"#,
            )
        );
        println!("{}", saved[saved.find("[ui]").unwrap()..].trim_end());
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 110, 24), &["1 agy", "2 shell", "8 pi"]);
        press(&mut app, KeyCode::Down);
        app.move_tool(true, &path(), installed);
        assert_eq!(load().unwrap()[0].name, "shell");
        std::fs::remove_dir_all(path().parent().unwrap()).unwrap();
    }

    #[test]
    fn agy_menu_shows_used_quota_on_desktop_and_phone() {
        let mut app = fixture()
            .with_tools(|| {
                let row = |name, tool, account, agent| Tool { note: String::new(), ..crate::ui::tests::tool(name, tool, account, agent) };
                Ok(vec![row("agy", "agy", "", Agent::Agy), row("sub", "claude", "sub", Agent::Claude)])
            })
            .with_rehome(
                |agent| match agent {
                    Agent::Agy => vec![crate::rehome::test_agy_quota()],
                    Agent::Claude => vec![aca_records::usage::Quota { agent, who: "sub".into(), ..crate::rehome::test_agy_quota() }],
                    _ => vec![],
                },
                |_, _| Login::In(String::new()),
                false,
            );
        press(&mut app, KeyCode::Char('n'));
        for w in [110, 40] {
            let rows = reader_rows(&mut app, w, 18);
            shows(&rows, &["选择工具"]);
            let agy = rows.iter().position(|line| line.contains("Antigravity")).unwrap();
            let sub = rows.iter().position(|line| line.contains("2 sub")).unwrap();
            shows(&rows[agy..sub], &["5h 1% · 7d 1%"]);
            if w == 110 {
                assert_eq!(rows.iter().filter(|line| line.contains("5h 1% · 7d 1%")).count(), 2);
            }
            assert!(!rows.iter().any(|line| line.contains("剩余")));
            println!("n {w}\n{}", rows.join("\n"));
        }
    }
    use crate::ui::tests::{
        ctrl, fake_source, fixture, handoff_tools, hints, keys, pin, press, reader_rows, sandbox_tools, select, sessions, shows,
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

    #[test]
    fn the_key_that_opened_a_tool_menu_closes_it() {
        let mut app = fixture().with_tools(sandbox_tools).with_handoff(fake_source).with_group_hints(hints);
        app.set_sessions(sessions());
        select(&mut app, "跑");
        // n closes n's; ^E switched to from there goes back to n's, then n closes
        keys(&mut app, "n");
        ctrl(&mut app, 'e');
        assert!(app.tool_special);
        ctrl(&mut app, 'e');
        assert!(!app.tool_special && app.tool_menu.is_some());
        keys(&mut app, "n");
        assert!(app.tool_menu.is_none() && app.new_draft.is_none() && !app.quit);
        // ^E opened from the list closes with ^E
        ctrl(&mut app, 'e');
        ctrl(&mut app, 'e');
        assert!(app.tool_menu.is_none() && !app.tool_special);
        // H closes the hand off (even from its special list), and n there does nothing
        keys(&mut app, "Hn");
        assert!(app.tool_menu.is_some() && app.handoff.is_some());
        ctrl(&mut app, 'e');
        keys(&mut app, "H");
        assert!(app.tool_menu.is_none() && app.handoff.is_none() && app.job.is_none());
    }
}
