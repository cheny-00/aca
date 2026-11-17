//! The billing page (`$`): what each account cost over a range, read in the background, and the picker for which accounts it counts.

use super::*;

/// `$` billing panel (content in `bill`, data in `usage`)
#[derive(Default)]
pub(super) struct BillView {
    /// Selected period (index into usage::RANGES)
    pub(super) sel: usize,
    /// Rows from the top
    pub(super) scroll: usize,
    /// Rows per screen when last drawn (pages by this)
    view_h: usize,
    /// The a account-picker float is open
    pick: Option<BillPick>,
    /// m: the folded-away blocks (usage, weekday × hour, by directory) are laid out
    more: bool,
}

/// The account-picker float: row 0 is 「全部」 ("all"), then each account (see `usage::Report::accounts`)
#[derive(Default)]
struct BillPick {
    sel: usize,
    /// Checked ones (multi-select); none checked means only the highlighted one
    marked: Vec<String>,
    /// v / V pressed, range-selecting: from this row to the highlighted one (as in the main list: Esc settles it into marked; v / V again
    /// starts over from the highlighted row)
    anchor: Option<usize>,
    /// This range is V unchecking
    unpick: bool,
}

impl BillPick {
    /// The chosen accounts: the checked ones plus the range being selected (minus it for an unchecking range), in the float's order
    /// (bigger spenders first)
    fn chosen(&self, accts: &[aca_records::usage::Row]) -> Vec<String> {
        let range = self.anchor.map(|a| a.min(self.sel)..=a.max(self.sel));
        let ranged = |i: usize| range.as_ref().is_some_and(|r| r.contains(&(i + 1)));
        accts
            .iter()
            .enumerate()
            .filter(|&(i, a)| match self.unpick {
                true => self.marked.contains(&a.name) && !ranged(i),
                false => self.marked.contains(&a.name) || ranged(i),
            })
            .map(|(_, a)| a.name.clone())
            .collect()
    }
}

impl App {
    pub(super) fn open_bill(&mut self) {
        self.bill = Some(BillView { sel: 1, ..Default::default() });
        self.bill_want = true;
    }

    /// Asked by the run loop: whether the background should read a fresh copy (if so, which accounts to show). Once a minute
    /// while the panel is open, at once on r or an account switch; only one read at a time
    pub fn bill_request(&mut self) -> Option<Vec<String>> {
        if self.bill.is_none() || self.bill_loading {
            return None;
        }
        let stale = self.bill_got.is_none_or(|t| t.elapsed() >= BILL_EVERY);
        if std::mem::take(&mut self.bill_want) || stale {
            self.bill_loading = true;
            return Some(self.bill_only.clone());
        }
        None
    }

    /// The background read is done; on failure keep the last numbers and say so in the bottom bar
    pub fn set_bill(&mut self, r: Result<aca_records::usage::Report, String>) {
        self.bill_got = Some(Instant::now());
        self.bill_loading = false;
        match r {
            Ok(r) => self.bill_data = Some(r),
            Err(e) => self.flash(&tr!("读取计费失败:{e}", "couldn't read billing: {e}")),
        }
    }

    /// Accounts in the account picker (from row 1): every account in the latest numbers, biggest spenders first
    fn bill_accounts(&self) -> Vec<aca_records::usage::Row> {
        self.bill_data.as_ref().map(|r| r.accounts.clone()).unwrap_or_default()
    }

    /// a: open the account picker, the highlight on the first account shown now ("All" if all)
    fn open_bill_pick(&mut self) {
        let accts = self.bill_accounts();
        if accts.is_empty() {
            return self.flash(t!("尚未读取到账号", "no accounts read yet"));
        }
        let sel = self.bill_only.first().and_then(|o| accts.iter().position(|a| &a.name == o)).map_or(0, |i| i + 1);
        if let Some(b) = self.bill.as_mut() {
            b.pick = Some(BillPick { sel, ..Default::default() });
        }
    }

    /// Switch to showing only these accounts (empty = all): close the picker, back to the top, have the background reread for it
    fn set_bill_only(&mut self, only: Vec<String>) {
        if let Some(b) = self.bill.as_mut() {
            b.pick = None;
            if only != self.bill_only {
                b.scroll = 0;
            }
        }
        if only != self.bill_only {
            self.bill_only = only;
            self.bill_want = true;
        }
    }

    /// Keys in the account picker: ↑↓ pick, ⏎ / a digit shows only it (with some ticked, shows the ticked ones), Space ticks / unticks,
    /// v range select, V range untick (↑↓ extends, Esc fixes, same as the main list), 0 all, Esc / a / q close
    fn on_bill_pick_key(&mut self, k: KeyEvent) {
        let accts = self.bill_accounts();
        let Some(p) = self.bill.as_mut().and_then(|b| b.pick.as_mut()) else { return };
        let n = accts.len() + 1;
        let ranging = p.anchor.is_some();
        match k.code {
            // While ranging: only move among accounts (don't wrap to "All")
            KeyCode::Up | KeyCode::Char('k') if ranging => p.sel = (p.sel - 1).max(1),
            KeyCode::Down | KeyCode::Char('j') if ranging => p.sel = (p.sel + 1).min(n - 1),
            KeyCode::Esc if ranging => {
                p.marked = p.chosen(&accts);
                p.anchor = None;
            }
            KeyCode::Esc | KeyCode::Char('a' | 'q') => {
                if let Some(b) = self.bill.as_mut() {
                    b.pick = None;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => p.sel = (p.sel + n - 1) % n,
            KeyCode::Down | KeyCode::Char('j') => p.sel = (p.sel + 1) % n,
            KeyCode::Char(c @ ('v' | 'V')) if n > 1 => {
                p.sel = p.sel.max(1);
                p.unpick = c == 'V';
                p.anchor = Some(p.sel);
            }
            KeyCode::Char(' ') => match accts.get(p.sel.wrapping_sub(1)) {
                Some(a) => {
                    match p.marked.iter().position(|m| *m == a.name) {
                        Some(i) => drop(p.marked.remove(i)),
                        None => p.marked.push(a.name.clone()),
                    }
                    p.sel = if ranging { (p.sel + 1).min(n - 1) } else { (p.sel + 1) % n };
                }
                None => p.marked.clear(),
            },
            KeyCode::Enter => {
                let chosen = p.chosen(&accts);
                let only = if !chosen.is_empty() {
                    chosen
                } else {
                    accts.get(p.sel.wrapping_sub(1)).map(|a| vec![a.name.clone()]).unwrap_or_default()
                };
                self.set_bill_only(only);
            }
            KeyCode::Char('0') => self.set_bill_only(vec![]),
            KeyCode::Char(c @ '1'..='9') => {
                if let Some(a) = accts.get(c as usize - '1' as usize) {
                    self.set_bill_only(vec![a.name.clone()]);
                }
            }
            _ => {}
        }
    }

    pub(super) fn on_bill_key(&mut self, k: KeyEvent) {
        if k.code == KeyCode::Char('l') && k.modifiers.contains(KeyModifiers::CONTROL) {
            return self.ask_relaunch();
        }
        if self.bill.as_ref().is_some_and(|b| b.pick.is_some()) {
            return self.on_bill_pick_key(k);
        }
        if k.code == KeyCode::Char('r') {
            self.bill_want = true;
            return;
        }
        if k.code == KeyCode::Char('a') {
            return self.open_bill_pick();
        }
        let Some(b) = self.bill.as_mut() else { return };
        let page = b.view_h.max(1);
        let n = aca_records::usage::RANGES.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q' | '$') => self.bill = None,
            KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => b.sel = (b.sel + 1) % n,
            KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => b.sel = (b.sel + n - 1) % n,
            KeyCode::Char(c @ '1'..='4') => b.sel = c as usize - '1' as usize,
            KeyCode::Char('m') => b.more = !b.more,
            KeyCode::Up | KeyCode::Char('k') => b.scroll = b.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => b.scroll = b.scroll.saturating_add(1),
            KeyCode::PageUp | KeyCode::Char('b') => b.scroll = b.scroll.saturating_sub(page),
            KeyCode::PageDown | KeyCode::Char(' ') => b.scroll = b.scroll.saturating_add(page),
            KeyCode::Home | KeyCode::Char('g') => b.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => b.scroll = usize::MAX, // clamped to the bottom when drawn
            _ => {}
        }
    }
}

/// Run `aca ui --cost-json` to read billing (same program, environment passed as is); only = which accounts to show
pub(super) fn read_bill(only: &[String]) -> Result<aca_records::usage::Report, String> {
    let exe = aca_core::agent::self_path().ok_or(t!("找不到 aca 程序", "can't find the aca program"))?;
    let out = std::process::Command::new(exe)
        .args(["ui", "--cost-json"])
        .args(only.iter().flat_map(|o| ["--only", o.as_str()]))
        // Use this side's current language (if the settings page just switched it, ACA_LANG may still be the other)
        .env("ACA_LANG", i18n::lang().code())
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    // Upgraded without reopening, the child is the new version and the format doesn't match: say so, reopening fixes it
    serde_json::from_slice(&out.stdout)
        .map_err(|_| t!("数据格式有误(如果刚升级过 aca,按 ^L 重启)", "unexpected format (if aca was upgraded, reopen with ^L)").to_string())
}

/// $ billing panel: full screen, the top says what time the numbers are from, content in the middle (see `bill`), keys at the bottom
pub(super) fn draw_bill(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let (w, h) = (area.width as usize, body.height as usize);
    let Some(b) = app.bill.as_mut() else { return };
    let lines = app.bill_data.as_ref().map(|r| crate::bill::lines(r, b.sel, b.more, w)).unwrap_or_default();
    let more = b.more;
    b.scroll = b.scroll.min(lines.len().saturating_sub(h));
    // Don't mention scrolling when it fits one screen
    let long = lines.len() > h;
    b.view_h = h;
    let scroll = b.scroll;
    let right = match (&app.bill_data, app.bill_loading) {
        (None, _) => t!("读取中…", "loading…").to_string(),
        (Some(r), _) if r.only != app.bill_only => t!("读取中…", "loading…").to_string(),
        (Some(_), true) => t!("更新中…", "updating…").to_string(),
        (Some(r), false) => {
            let t = (r.now + r.off).rem_euclid(86400);
            format!("{:02}:{:02}", t / 3600, t % 3600 / 60)
        }
    };
    // Which accounts are shown goes after the title (by the data shown)
    let title = match app.bill_data.as_ref().map(|r| &r.only) {
        Some(o) if !o.is_empty() => tr!("计费 · {}", "Billing · {}", o.join(" + ")),
        _ => t!("计费", "Billing").into(),
    };
    f.render_widget(Paragraph::new(title_rule(("$", theme::accent()), &title, vec![Span::styled(right, dim())], w)), head);
    if lines.is_empty() {
        f.render_widget(Paragraph::new(t!(" 正在读取对话记录…", " reading the conversation record…")).style(dim()), body);
    } else {
        f.render_widget(Paragraph::new(lines.into_iter().skip(scroll).take(h).collect::<Vec<_>>()), body);
    }
    let foot_line = match &app.msg {
        Some((m, t)) if t.elapsed() < app.msg_ttl => Line::styled(m.clone(), theme::warn()).centered(),
        _ => {
            let mut keys = vec![
                ("←→", t!("时段", "period"), 0),
                ("a", t!("账号", "accounts"), 1),
                ("m", if more { t!("收起", "less") } else { t!("更多", "more") }, 1),
                ("↑↓", t!("滚动", "scroll"), 2),
                ("r", t!("重新读取", "reload"), 1),
            ];
            keys.retain(|k| long || k.0 != "↑↓");
            menu_bar(&[keys, vec![("Esc", t!("返回", "back"), 0)]], None, w)
        }
    };
    f.render_widget(Paragraph::new(foot_line), foot);
    if let Some(sel) = app.bill.as_ref().and_then(|b| b.pick.as_ref()).map(|p| p.sel) {
        draw_bill_pick(f, app, sel);
    }
}

/// a's account picker floating window: first row 「全部」 ("all"), then one account per row (number, tick, name colored by provider,
/// spend over all time); the numbers of the ones shown now in accent color, selected ones ticked, the highlighted row gets a background;
/// during a range select the keys at the bottom become range-select ones
fn draw_bill_pick(f: &mut Frame, app: &App, sel: usize) {
    let accts = app.bill_accounts();
    let pick = app.bill.as_ref().and_then(|b| b.pick.as_ref());
    let marked = pick.map(|p| p.chosen(&accts)).unwrap_or_default();
    let ranging = pick.is_some_and(|p| p.anchor.is_some());
    let total: f64 = accts.iter().map(|a| a.cost).sum();
    modal(f, 48, theme::pal().accent, |w| {
        let mut out = vec![
            modal_title(
                t!("按账号筛选", "Which accounts"),
                theme::accent(),
                esc_hint(if ranging { t!("确定", "keep") } else { t!("关闭", "close") }),
                w,
            ),
            Line::raw(""),
        ];
        let rows = std::iter::once(("0".to_string(), None))
            .chain(accts.iter().enumerate().map(|(i, a)| (if i < 9 { (i + 1).to_string() } else { " ".into() }, Some(a))));
        for (i, (key, a)) in rows.enumerate() {
            let on = match a {
                None => app.bill_only.is_empty(),
                Some(a) => app.bill_only.contains(&a.name),
            };
            let key_st = if on { theme::accent().add_modifier(Modifier::BOLD) } else { key_style() };
            let ticked = a.is_some_and(|a| marked.contains(&a.name));
            let green = Style::new().fg(theme::pal().green).add_modifier(Modifier::BOLD);
            let tick = if ticked { Span::styled("✓ ", green) } else { Span::raw("  ") };
            let (name, st, cost) = match a {
                None => (t!("全部", "all").to_string(), Style::new(), total),
                Some(a) => (a.name.clone(), agent_style(a.agent), a.cost),
            };
            let money = crate::bill::money(cost);
            let name_w = w.saturating_sub(1 + 2 + 2 + 2 + money.width());
            let mut line = Line::from(vec![
                Span::styled(key, key_st),
                Span::raw("  "),
                tick,
                Span::styled(clip(&name, name_w), st.add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(money, dim()),
            ]);
            // The highlighted one on light purple, ticked ones on light green (like v in the main list)
            let bg = if i == sel { Some(theme::pal().sel) } else { ticked.then(|| theme::pal().pick) };
            if let Some(bg) = bg {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(bg));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        let keys: &[(&str, &str)] = if ranging && pick.is_some_and(|p| p.unpick) {
            &[("↑↓", t!("取消选中", "range unselect")), ("⏎", t!("查看已选", "show selected"))]
        } else if ranging {
            &[("↑↓", t!("连选", "range")), ("⏎", t!("查看已选", "show selected"))]
        } else {
            &[
                (t!("⏎ 数字", "⏎ digit"), t!("仅显示此项", "only it")),
                ("v", t!("多选", "multi")),
                (t!("空格", "Space"), t!("选中", "tick")),
                ("0", t!("全部", "all")),
            ]
        };
        out.push(keys_line(keys, vec![], w));
        out
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{ctrl, keys, press, reader_rows, sessions, shows};

    #[test]
    fn ctrl_l_relaunches_aca_unless_something_is_still_running() {
        let mut app = App::new(String::new(), sessions());
        app.busy = Some("正在重启「闲」…".into());
        ctrl(&mut app, 'l');
        assert!(!app.relaunch && !app.quit);
        assert_eq!(app.msg.as_ref().unwrap().0, "请稍候:正在重启「闲」…");
        app.busy = None;
        ctrl(&mut app, 'l');
        assert!(app.relaunch && app.quit);
        assert_eq!(app.relaunch_state(), "list");
        // Works in the billing panel too (it's not a period switch); remembers which period was open
        let mut app = App::new(String::new(), sessions());
        press(&mut app, KeyCode::Char('$'));
        press(&mut app, KeyCode::Right);
        ctrl(&mut app, 'l');
        assert!(app.relaunch && app.quit);
        assert_eq!(app.relaunch_state(), "bill:2");
        // The new one: reopens the billing panel and says when it was installed
        let mut app = App::new(String::new(), sessions());
        app.resume("bill:2", Some("11:28".into()));
        assert_eq!(app.bill.as_ref().map(|b| b.sel), Some(2));
        assert_eq!(app.msg.as_ref().unwrap().0, "已重启 · 当前 aca 安装于 11:28");
        let mut app = App::new(String::new(), sessions());
        app.resume("list", None);
        assert!(app.bill.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "已重启");
    }

    #[test]
    fn m_on_the_bill_panel_shows_and_hides_the_folded_blocks() {
        let mut app = App::new(String::new(), sessions());
        press(&mut app, KeyCode::Char('$'));
        shows(&reader_rows(&mut app, 80, 10), &["m 更多"]);
        press(&mut app, KeyCode::Char('m'));
        assert!(app.bill.as_ref().unwrap().more && app.msg.is_none());
        shows(&reader_rows(&mut app, 80, 10), &["m 收起"]);
        press(&mut app, KeyCode::Char('m'));
        assert!(!app.bill.as_ref().unwrap().more);
    }

    #[test]
    fn a_picks_which_accounts_the_bill_panel_counts() {
        use aca_records::usage::{Report, Row};
        let mut app = App::new(String::new(), sessions());
        press(&mut app, KeyCode::Char('$'));
        assert_eq!(app.bill_request(), Some(vec![]));
        let row = |name: &str, agent, cost| Row { name: name.into(), agent, cost, ..Default::default() };
        let accounts =
            vec![row("Codex big", Agent::Codex, 30.0), row("Claude mid", Agent::Claude, 20.0), row("Claude small", Agent::Claude, 1.0)];
        app.set_bill(Ok(Report { accounts: accounts.clone(), ..Default::default() }));
        app.bill.as_mut().unwrap().scroll = 5;
        // a opens with the highlight on 「全部」 ("all"); ↓ ⏎ = show only the first, back to the top, reread for it right away
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.bill.as_ref().unwrap().pick.as_ref().map(|p| p.sel), Some(0));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(app.bill.as_ref().unwrap().pick.is_none());
        assert_eq!(app.bill.as_ref().unwrap().scroll, 0);
        assert_eq!(app.bill_request(), Some(vec!["Codex big".to_string()]));
        // Not read back yet: the top still shows the data on display
        assert_eq!(app.bill_request(), None);
        app.set_bill(Ok(Report { only: vec!["Codex big".into()], accounts: accounts.clone(), ..Default::default() }));
        // Reopened, the highlight is on the one shown; Space ticks two (moving down after each), ⏎ shows the ticked ones, sorted by cost
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.bill.as_ref().unwrap().pick.as_ref().map(|p| p.sel), Some(1));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.bill.as_ref().unwrap().pick.as_ref().unwrap().marked, vec!["Claude small".to_string(), "Claude mid".to_string()]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.bill_only, vec!["Claude mid".to_string(), "Claude small".to_string()]);
        // In the floating window 1-4 don't switch periods, they pick accounts directly; 0 = all; Esc changes nothing
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('3'));
        assert_eq!((app.bill_only.clone(), app.bill.as_ref().unwrap().sel), (vec!["Claude small".to_string()], 1));
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Esc);
        assert!(app.bill.is_some() && app.bill_only == vec!["Claude small".to_string()]);
        // The restart carries which ones are shown, and the new one shows just those
        assert_eq!(app.relaunch_state(), "bill:1\tClaude small");
        let mut again = App::new(String::new(), sessions());
        again.resume(&app.relaunch_state(), None);
        assert_eq!(again.bill_only, vec!["Claude small".to_string()]);
        assert_eq!(again.bill_request(), Some(vec!["Claude small".to_string()]));
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('0'));
        assert!(app.bill_only.is_empty());
        // Closed and reopened, it still shows only those (here: all)
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('$'));
        assert!(app.bill_only.is_empty());
    }

    #[test]
    fn v_in_the_account_picker_selects_a_run_of_rows() {
        use aca_records::usage::{Report, Row};
        let mut app = App::new(String::new(), sessions());
        press(&mut app, KeyCode::Char('$'));
        let row = |name: &str| Row { name: name.into(), agent: Agent::Claude, cost: 1.0, ..Default::default() };
        app.set_bill(Ok(Report { accounts: vec![row("A"), row("B"), row("C"), row("D")], ..Default::default() }));
        let pick = |app: &App| app.bill.as_ref().unwrap().pick.as_ref().map(|p| (p.sel, p.anchor, p.chosen(&app.bill_accounts())));
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // v on 「全部」 ("all"): starts from the first account; going up doesn't wrap back to 「全部」
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Up);
        assert_eq!(pick(&app), Some((1, Some(1), names(&["A"]))));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(pick(&app), Some((3, Some(1), names(&["A", "B", "C"]))));
        // v again during a range select: the unsettled range is dropped, starting over from the highlighted row; Esc settles it, the
        // floating window stays open
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(pick(&app), Some((3, Some(3), names(&["C"]))));
        press(&mut app, KeyCode::Esc);
        assert_eq!(pick(&app), Some((3, None, names(&["C"]))));
        // v from a ticked row still range-selects; V range-unticks
        keys(&mut app, "kkvjj");
        press(&mut app, KeyCode::Esc);
        assert_eq!(pick(&app), Some((3, None, names(&["A", "B", "C"]))));
        keys(&mut app, "kVj");
        assert_eq!(pick(&app), Some((3, Some(2), names(&["A"]))));
        press(&mut app, KeyCode::Esc);
        // Space during a range select: ticks this one and moves down, still range-selecting
        // (at the last it doesn't wrap back to 「全部」); ⏎ shows the selected
        keys(&mut app, "v ");
        assert_eq!(pick(&app), Some((4, Some(3), names(&["A", "C", "D"]))));
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(pick(&app).unwrap().0, 4);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.bill_only, names(&["A", "C", "D"]));
        assert!(app.bill.as_ref().unwrap().pick.is_none());
    }
}
