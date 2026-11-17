//! Starting a new session (`N`, or after picking a tool in `n`): the path being typed, its candidates and the directory picker.

use super::*;

impl App {
    /// The path defaults to the selected session's directory — the list itself is the directory picker; mostly just Enter, no typing
    /// paths on a phone
    /// Start of the fzf candidates: the selected session's directory, then the other sessions' in the list
    pub fn dir_hints(&self) -> Vec<String> {
        let first = self.current().map(|s| s.dir.clone());
        first.clone().into_iter().chain(self.view.iter().map(|&i| self.all[i].dir.clone()).filter(|d| Some(d) != first.as_ref())).collect()
    }

    /// The fzf opened by ↓ / ^F: candidate directories, each marked with an icon, which group it goes to, and sessions already in it.
    /// Scans directories and asks zoxide (~0.1s), so prepared before giving up the terminal, to avoid showing the shell underneath
    pub fn dir_pick(&self, width: u16, height: u16) -> Option<DirPick> {
        let base = self.new_draft.as_ref()?.base.clone();
        self.dir_pick_from(create::dir_candidates(base.as_deref(), &self.dir_hints()), width, height)
    }

    pub(super) fn dir_pick_from(&self, cands: Vec<(String, Src)>, width: u16, height: u16) -> Option<DirPick> {
        let d = self.new_draft.as_ref()?;
        let rows = cands
            .into_iter()
            .map(|(dir, src)| {
                // Count only this workspace's: other workspaces' titles (h especially) shouldn't show here
                let here: Vec<&Session> =
                    self.all.iter().filter(|s| s.space == self.space && !s.dir.is_empty() && words::tilde(&s.dir) == dir).collect();
                let mark = match src {
                    _ if !here.is_empty() => '●',
                    Src::Known => '◇',
                    Src::Zoxide => '↗',
                    Src::Near => ' ',
                };
                DirRow {
                    group: d.group_at(&dir),
                    sessions: here.iter().map(|s| (session_icon(s).0.to_string(), s.title.clone())).collect(),
                    dir,
                    mark,
                }
            })
            .collect();
        Some(DirPick { rows, what: d.label.clone(), query: d.pick_query(), width, height })
    }

    /// fzf is back: Enter / double click opens there; → fills the path box to
    /// keep typing; Esc changes nothing; errors go below the input box
    pub fn picked(&mut self, got: Result<Option<Picked>, String>) {
        let Some(d) = self.new_draft.as_mut() else { return };
        match got {
            Ok(Some(Picked::Edit(dir))) => d.set_dir(&dir),
            Ok(Some(Picked::Open(dir))) => {
                d.set_dir(&dir);
                match d.finish() {
                    Ok(r) => {
                        self.create = Some(r);
                        self.new_draft = None;
                    }
                    Err(e) => d.hint = e,
                }
            }
            Ok(None) => {}
            Err(e) => d.hint = e,
        }
    }

    pub(super) fn ask_new(&mut self) {
        match self.draft() {
            Ok(d) => {
                self.new_draft = Some(d);
                self.pick = self.pick_first();
            }
            Err(e) => self.flash(&e),
        }
    }

    /// Where N / n start filling from: on a session, another one like it; on a group, the group's path (see `group_dir`), into
    /// this group. Empty group: into this group (it stays even if the path changes), starting from the group's default directory,
    /// or if none recorded, from the new group directory like ^N
    pub(super) fn draft(&self) -> Result<Draft, String> {
        if let Some(g) = self.blank_group() {
            let mut d = Draft::from(None, self.group_hints);
            let base = self.group_dir(g).map_or_else(|| (self.new_group_dir)(), |(_, dir, _)| dir);
            d.start_group(g.to_string(), &base);
            return Ok(d);
        }
        match self.group_row().and_then(|g| Some((g, self.group_dir(g)?))) {
            Some((g, (s, dir, _))) => Ok(Draft::in_group(s, g, &dir, self.group_hints)),
            None => Ok(Draft::from(self.current(), self.group_hints)),
        }
    }

    /// Typing (filter words / a new session's path): several keys arriving at once
    /// then is IME suggestions or a paste, not Moshi typing on your behalf
    pub fn typing(&self) -> bool {
        self.filtering
            || self.new_draft.is_some()
            || self.prompt.is_some()
            || self.term_on.is_some()
            || self.overlays.last().is_some_and(|o| o.typing())
    }

    pub(super) fn on_draft_key(&mut self, k: KeyEvent) {
        let Some(d) = self.new_draft.as_mut() else { return };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Enter => match d.finish() {
                Ok(r) => {
                    self.create = Some(r);
                    self.new_draft = None;
                }
                Err(e) => d.hint = e,
            },
            KeyCode::Esc => self.new_draft = None,
            KeyCode::Tab => d.complete(),
            KeyCode::BackTab => d.complete_back(),
            KeyCode::Right => d.accept(),
            KeyCode::Down => self.pick = true,
            KeyCode::Char('f') if ctrl => self.pick = true,
            KeyCode::Backspace => d.backspace(),
            KeyCode::Char('u') if ctrl => d.clear(),
            KeyCode::Char('w') if ctrl => d.kill_segment(),
            KeyCode::Char('t') if ctrl => d.shell = !d.shell,
            KeyCode::Char(c) if !ctrl => d.push(c),
            _ => {}
        }
    }
}

/// Max rows of candidates listed for new
const DRAFT_ROWS: usize = 8;

/// `N`'s new popup: which tool, which group it lands in (follows the path), the directory input box, below it Tab-completion candidates
pub(super) fn draw_draft(f: &mut Frame, d: &Draft) {
    modal(f, 78, theme::pal().accent, |w| {
        // The label column: Chinese labels are all two chars (4 cells); English ones are padded to the same width
        let lab = |t: &str| Span::styled(format!("{t}{}", " ".repeat(7usize.saturating_sub(t.width()).max(2))), dim());
        let shell = d.is_shell();
        let tool = match () {
            // A shell inside a sandbox, picked in n (^T switches to the plain one)
            _ if shell && !d.shell && !d.account.is_empty() => format!("shell:{}", d.account),
            _ if shell => "shell".to_string(),
            _ if d.label.is_empty() => t!("默认", "default").to_string(),
            _ => d.label.clone(),
        };
        let tool_style = match d.label.split('·').next() {
            _ if shell => Style::new(),
            Some("Claude") => agent_style(Agent::Claude),
            Some("Codex") => agent_style(Agent::Codex),
            Some("Pi") => agent_style(Agent::Pi),
            Some("OpenCode") => agent_style(Agent::Opencode),
            _ => Style::new(),
        };
        // ^T switches between the reused tool and a shell (no need if shell was picked in n)
        let swap = match () {
            _ if d.shell => t!("^T 换回", "^T back"),
            _ if shell => "",
            _ => "^T shell",
        };
        let (group, new) = d.group();
        let mut group_line = vec![lab(t!("分组", "group"))];
        if group.is_empty() {
            group_line.push(Span::styled(t!("默认", "default"), dim()));
        } else {
            group_line.push(Span::styled(clip(&group, 24).trim_end().to_string(), Style::new().add_modifier(Modifier::BOLD)));
            if new {
                group_line.push(Span::styled(
                    if d.binds_dir() {
                        t!("  新分组 · 绑定目录", "  new group · bound to the directory")
                    } else {
                        t!("  新分组", "  new group")
                    },
                    theme::warn(),
                ));
            }
        }
        let mark = if d.is_new_dir() { vec![Span::styled(t!("新目录", "new directory"), theme::warn())] } else { vec![] };
        let mut out = vec![
            modal_title(t!("新建会话", "New session"), theme::accent(), esc_hint(t!("取消", "cancel")), w),
            Line::raw(""),
            spread(
                vec![
                    lab(t!("工具", "tool")),
                    Span::styled(
                        clip(&tool, w.saturating_sub(7 + swap.width() + 2)).trim_end().to_string(),
                        tool_style.add_modifier(Modifier::BOLD),
                    ),
                ],
                vec![Span::styled(swap.to_string(), dim())],
                w,
            ),
            Line::from(group_line),
            Line::raw(""),
        ];
        let field = w.saturating_sub(7);
        let mut input = input_row(&d.text, mark, field);
        input.spans.insert(0, lab(t!("目录", "dir")));
        out.push(input);
        out.push(Line::from(vec![Span::raw(" ".repeat(7)), Span::styled("─".repeat(field), dim())]));
        if !d.menu.is_empty() {
            let sel = d.sel.unwrap_or(0);
            let start = (sel + 1).saturating_sub(DRAFT_ROWS).min(d.menu.len().saturating_sub(DRAFT_ROWS));
            for (i, m) in d.menu.iter().enumerate().skip(start).take(DRAFT_ROWS) {
                let mut line = Line::from(vec![Span::raw(" ".repeat(7)), Span::raw(clip(m, field).trim_end().to_string())]);
                if d.sel == Some(i) {
                    let pad = w.saturating_sub(line.width());
                    line.spans.push(Span::raw(" ".repeat(pad)));
                    line = line.patch_style(Style::new().bg(theme::pal().sel).add_modifier(Modifier::BOLD));
                }
                out.push(line);
            }
            let more = d.menu.len().saturating_sub(DRAFT_ROWS);
            if more > 0 {
                out.push(Line::styled(tr!("{}… 还有 {more} 项", "{}… {more} more", " ".repeat(7)), dim()));
            }
        } else if !d.hint.is_empty() {
            out.push(Line::styled(format!("{}{}", " ".repeat(7), clip(&d.hint, field).trim_end()), theme::warn()));
        }
        out.push(Line::raw(""));
        out.push(keys_line(
            &[("Tab", t!("补全", "complete")), ("↓", "fzf")],
            vec![button(t!("⏎ 新建", "⏎ create"), theme::pal().accent)],
            w,
        ));
        out
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{
        ctrl, fixture, fun_in_h, keys, mk, pin, press, reader_rows, select, sessions, shows, target, tool, typ, with_windows,
    };

    #[test]
    fn new_defaults_to_the_selected_sessions_dir_and_group() {
        let mut app = fixture();
        pin(&mut app, "跑"); // select 「跑」 (work group)
        press(&mut app, KeyCode::Char('N'));
        assert_eq!(app.new_draft.as_ref().unwrap().text, "/p/work");
        assert!(app.typing());
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.dir.as_str(), r.group.as_str(), r.spec().as_str()), ("/p/work", "work", "claude"));
        assert!(r.create_dir); // /p/work doesn't exist
        assert!(app.attach.is_none()); // Enter went to the input box, didn't go into the original session
    }

    #[test]
    fn new_inherits_tool_and_account_from_the_selected_session() {
        let mut v = sessions();
        v[2].tool = "codex".into(); // 「跑」
        v[2].account = "team".into();
        let mut app = App::new(String::new(), v);
        pin(&mut app, "跑");
        press(&mut app, KeyCode::Char('N'));
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.tool.as_str(), r.account.as_str()), ("codex", "team"));
        pin(&mut app, "闲"); // claude on the default account
        press(&mut app, KeyCode::Char('N'));
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.tool.as_str(), r.account.as_str()), ("claude", ""));
    }

    #[test]
    fn down_or_ctrl_f_in_new_asks_for_fzf_and_fills_the_path() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('j')); // select 「跑」
        assert_eq!(app.dir_hints(), ["/p/work", "/p/fun"]); // the selected one first, same directories not repeated
        press(&mut app, KeyCode::Down); // ↓ in the list moves down, doesn't open fzf
        assert!(!app.pick);
        press(&mut app, KeyCode::Char('N'));
        press(&mut app, KeyCode::Down);
        assert!(std::mem::take(&mut app.pick));
        ctrl(&mut app, 'f');
        assert!(std::mem::take(&mut app.pick));
        app.picked(Ok(None)); // Esc: unchanged
        assert_eq!(app.new_draft.as_ref().unwrap().text, "/p/work");
        app.picked(Err("没装 fzf".into()));
        assert_eq!(app.new_draft.as_ref().unwrap().hint, "没装 fzf");
        app.picked(Ok(Some(Picked::Edit("~/code/app".into())))); // →: fill it back in and keep typing
        typ(&mut app, "new");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.create.take().unwrap().dir, format!("{}/code/app/new", std::env::var("HOME").unwrap()));
    }

    #[test]
    fn enter_in_fzf_opens_right_away() {
        let mut app = fixture().with_group_hints(|| crate::data::GroupHints { groups: vec![("fun".into(), String::new())] });
        press(&mut app, KeyCode::Char('N'));
        app.picked(Ok(Some(Picked::Open("/p/fun".into()))));
        assert!(app.new_draft.is_none());
        let r = app.create.take().unwrap();
        assert_eq!((r.dir.as_str(), r.group.as_str()), ("/p/fun", "fun"));
    }

    #[test]
    fn fzf_rows_mark_dirs_that_already_have_sessions() {
        let mut app = fixture().with_group_hints(|| crate::data::GroupHints { groups: vec![("work".into(), String::new())] });
        press(&mut app, KeyCode::Char('j')); // select 「跑」 (/p/work)
        press(&mut app, KeyCode::Char('N'));
        let cands = [("/p/work", Src::Known), ("/p/gone-quiet", Src::Known), ("/z/often", Src::Zoxide), ("/p/near", Src::Near)];
        let p = app.dir_pick_from(cands.map(|(d, s)| (d.to_string(), s)).to_vec(), 40, 16).unwrap();
        let marks: String = p.rows.iter().map(|r| r.mark).collect();
        assert_eq!(marks, "●◇↗ "); // ● has a session, ◇ registered but no session now
        assert_eq!(p.rows[0].group, ("work".into(), false));
        assert!(p.rows[0].sessions.iter().any(|(_, t)| t == "跑"), "{:?}", p.rows[0].sessions);
        assert_eq!(p.rows[3].group, ("near".into(), true));
        assert_eq!(p.query, "");
    }

    #[test]
    fn ctrl_t_in_new_swaps_the_tool_for_a_shell_and_back() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('N'));
        shows(&reader_rows(&mut app, 100, 30), &["^T shell"]);
        ctrl(&mut app, 't');
        shows(&reader_rows(&mut app, 100, 30), &["工具   shell", "^T 换回"]);
        assert_eq!(app.new_draft.as_ref().unwrap().finish().unwrap().tool, "shell");
        ctrl(&mut app, 't');
        assert!(!app.new_draft.as_ref().unwrap().is_shell());
        // Phone width: the hint and the tool name squeezed into one row still fit
        let rows = reader_rows(&mut app, 44, 30);
        assert!(rows.iter().any(|r| r.contains("^T shell")), "{rows:#?}");
    }

    #[test]
    fn tab_candidates_list_under_the_input_and_follow_the_pick() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('N'));
        let d = app.new_draft.as_mut().unwrap();
        d.menu = (0..10).map(|i| format!("item{i}/")).collect();
        d.sel = Some(9);
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer().clone();
        let rows = reader_rows(&mut app, 100, 30);
        // At most 8 listed, scrolling with the one switched to; that one's whole row is filled light purple
        assert!(!rows.iter().any(|r| r.contains("item1/")) && rows.iter().any(|r| r.contains("item2/")), "{rows:#?}");
        let y = rows.iter().position(|r| r.contains("item9/")).unwrap() as u16;
        let x = rows[y as usize].find("item9/").map(|b| rows[y as usize][..b].width() as u16).unwrap();
        assert_eq!(buf[(x, y)].bg, theme::pal().sel);
        assert!(rows.iter().any(|r| r.contains("还有 2 项")), "{rows:#?}");
    }

    #[test]
    fn new_session_group_changes_with_the_path() {
        let mut app = fixture()
            .with_group_hints(|| crate::data::GroupHints { groups: vec![("work".into(), String::new()), ("fun".into(), String::new())] });
        pin(&mut app, "跑"); // work group
        press(&mut app, KeyCode::Char('N'));
        assert_eq!(app.new_draft.as_ref().unwrap().group(), ("work".into(), false));
        app.picked(Ok(Some(Picked::Edit("/p/fun".into())))); // the directory name is the fun group
        assert_eq!(app.new_draft.as_ref().unwrap().group(), ("fun".into(), false));
        typ(&mut app, "lab"); // /p/fun/lab: parent is the fun group → nested
        assert_eq!(app.new_draft.as_ref().unwrap().group(), ("fun/lab".into(), true));
        app.picked(Ok(Some(Picked::Edit("/p/brand".into()))));
        let rows = reader_rows(&mut app, 60, 20);
        assert!(rows.iter().any(|r| r.contains("brand  新分组")), "{rows:#?}");
        press(&mut app, KeyCode::Enter);
        let req = app.create.take().unwrap();
        // Derived from the path: no ^N-style binding; left to core to claim by directory (recording the default directory)
        assert_eq!((req.group.as_str(), req.bind, req.by_dir), ("brand", false, true));
    }

    #[test]
    fn f_forks_the_session_and_highlights_the_new_one() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('f'));
        // Name it first: prefilled with the default, Enter uses it as is (not locked)
        assert!(app.job.is_none() && app.typing());
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.job, Some(Job::Fork { t: target("闲"), title: None }));
        let job = app.take_job().unwrap();
        assert_eq!(app.busy.as_deref(), Some("正在分叉「闲」…"));
        // The fork goes last in the group, the highlight follows it
        let mut fresh = sessions();
        let mut new = mk("闲-fork", "work", State::Running, 0);
        new.slot.1 = 99;
        new.deck_id = "id-新".into();
        fresh.push(new);
        app.job_done(&job, Outcome::Made { msg: "已分出「闲-fork」".into(), id: "id-新".into() }, fresh);
        assert_eq!(app.current().unwrap().title, "闲-fork");
        assert_eq!(app.msg.as_ref().unwrap().0, "已分出「闲-fork」");
        // On failure the highlight stays
        press(&mut app, KeyCode::Char('f'));
        press(&mut app, KeyCode::Enter);
        let job = app.take_job().unwrap();
        app.job_done(&job, Outcome::Failed("分叉失败:还没有对话记录".into()), sessions());
        assert_eq!(app.msg.as_ref().unwrap().0, "分叉失败:还没有对话记录");
    }

    #[test]
    fn t_opens_a_terminal_under_the_list_and_esc_puts_it_away() {
        let mut app = with_windows();
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('t'));
        let r = app.term_req.take().unwrap();
        assert_eq!((r.key.as_str(), r.family.as_str(), r.w.dir.as_str()), (app.current().unwrap().name.as_str(), "闲", "/p/work"));
        // While open, keys go to the terminal: q doesn't quit, ^C doesn't quit, j doesn't move; Esc hides it
        app.term_on = Some(r.key.clone());
        assert!(app.typing()); // paste / fast typing isn't dropped as automated input
        let at = app.current().unwrap().name.clone();
        press(&mut app, KeyCode::Char('q'));
        ctrl(&mut app, 'c');
        press(&mut app, KeyCode::Char('j'));
        assert!(!app.quit && app.current().unwrap().name == at);
        press(&mut app, KeyCode::Esc);
        assert!(app.term_on.is_none());
        press(&mut app, KeyCode::Char('q'));
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
        // A fold row is a group: opens at the group's path (see t_on_a_group_opens_a_terminal_in_the_groups_dir)
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('o'));
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.term_req.take().map(|r| r.key).as_deref(), Some("group:work"));
    }

    #[test]
    fn n_picks_a_tool_then_asks_for_the_path() {
        let mut app =
            fixture().with_tools(|| Ok(vec![tool("work", "claude", "work", Agent::Claude), tool("cx", "codex", "cx", Agent::Codex)]));
        pin(&mut app, "跑");
        pin(&mut app, "跑"); // select 「跑」
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 46, 12), &["选择工具", "1 work  ~/.work", "2 cx    ~/.cx", "⏎ 选择"]);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j')); // wraps around to the first
        press(&mut app, KeyCode::Esc);
        assert!(app.tool_menu.is_none() && app.new_draft.is_none());
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('2')); // a digit picks directly
        let d = app.new_draft.as_ref().unwrap();
        assert_eq!((d.label.as_str(), d.text.as_str()), ("cx", "/p/work"));
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.spec().as_str(), r.dir.as_str()), ("codex:cx", "/p/work"));
        // A row that can't start (the tool names an unregistered account): says why; picking it only shows a message, the
        // menu stays
        let mut bad = fixture().with_tools(|| {
            Ok(vec![Tool { problem: Some("没有 Codex 账号槽 gone".into()), ..tool("cx", "codex", "gone", Agent::Codex) }])
        });
        pin(&mut bad, "跑");
        pin(&mut bad, "跑");
        press(&mut bad, KeyCode::Char('n'));
        shows(&reader_rows(&mut bad, 46, 12), &["1 cx  没有 Codex 账号槽 gone"]);
        press(&mut bad, KeyCode::Char('1'));
        assert!(bad.tool_menu.is_some() && bad.new_draft.is_none());
        assert_eq!(bad.msg.as_ref().unwrap().0, "没有 Codex 账号槽 gone");
        // Empty menu: say where to change it
        let mut empty = fixture();
        press(&mut empty, KeyCode::Char('n'));
        assert!(empty.tool_menu.is_none() && empty.msg.as_ref().unwrap().0.starts_with("工具菜单为空"));
    }

    #[test]
    fn new_path_can_be_edited_or_cancelled() {
        let mut app = fixture();
        pin(&mut app, "跑");
        press(&mut app, KeyCode::Char('N'));
        typ(&mut app, "q"); // q in the input box is a letter, not quit
        assert!(!app.quit);
        press(&mut app, KeyCode::Esc);
        assert!(app.new_draft.is_none() && app.create.is_none());
        press(&mut app, KeyCode::Char('N'));
        ctrl(&mut app, 'w');
        typ(&mut app, "新项目");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.create.take().unwrap().dir, "/p/新项目");
        press(&mut app, KeyCode::Char('N'));
        ctrl(&mut app, 'u');
        press(&mut app, KeyCode::Enter); // empty path isn't created, the message stays above the input box
        assert!(app.create.is_none());
        assert_eq!(app.new_draft.as_ref().unwrap().hint, "路径不能为空");
    }

    #[test]
    fn keyword_lookup_and_fzf_rows_only_see_this_workspace() {
        let mut app = fun_in_h();
        assert!(app.matching("生活").is_empty());
        assert_eq!(app.matching("工作").len(), 1);
        keys(&mut app, "qh");
        assert_eq!(app.matching("生活").iter().filter(|s| s.sub.is_none()).count(), 2);
        // N's fzf: sessions in h don't show their titles among 1's candidates
        keys(&mut app, "q1");
        let mut app = app.with_group_hints(|| crate::data::GroupHints { groups: vec![("work".into(), String::new())] });
        press(&mut app, KeyCode::Char('N'));
        let p = app.dir_pick_from(vec![("/p/fun".into(), Src::Known), ("/p/work".into(), Src::Known)], 40, 16).unwrap();
        assert!(p.rows[0].sessions.is_empty() && p.rows[0].mark == '◇', "{:?}", p.rows[0].sessions);
        assert!(p.rows[1].sessions.iter().any(|(_, t)| t == "工作"));
    }
}
