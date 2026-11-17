//! What keys and the mouse do on the list (`on_key`, `on_mouse`); a page that is open gets its own keys first.

use super::*;

impl App {
    pub fn on_key(&mut self, k: KeyEvent) {
        // ^`: back to the previous session, like ^` in tmux. With a popup open tmux doesn't apply key bindings;
        // it turns it into NUL for aca, which arrives as ^Space (a real ^Space is taken by macOS to switch input method,
        // never gets here); in the keys popup it's only looked up
        if crate::data::in_popup()
            && k.code == KeyCode::Char(' ')
            && k.modifiers.contains(KeyModifiers::CONTROL)
            && !self.reader.as_ref().is_some_and(|r| r.help)
        {
            return self.back_session();
        }
        // A popup covers the terminal (popped when a login link appears, see browser.rs):
        // keys go to the popup first, back to the terminal once it's closed
        if self.term_on.is_some() && !self.overlays.is_empty() {
            return self.on_overlay_key(k);
        }
        if let Some(key) = self.term_on.clone() {
            return self.on_term_key(&key, k);
        }
        // ^\: the key that opened the popup, press it again to close (with a popup open tmux doesn't apply key bindings, so the key arrives
        // here). You stay on the session now showing beside; in the keys popup it's only looked up
        if (self.following.is_some() || crate::data::in_popup()) && closes_popup(k) && !self.reader.as_ref().is_some_and(|r| r.help) {
            self.quit = true;
            return;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        // Ctrl+Alt+letter is a key pressed inside a session (aca's own in tmux like Ctrl+Alt+N, and plugin hotkeys): reaching here
        // (the interface opened outside tmux, or a popup open, where tmux doesn't apply bindings) it does nothing. Matched by its
        // letter alone, Ctrl+Alt+B would open the trash
        if k.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && matches!(k.code, KeyCode::Char(_))
            && !self.ref_keys.first().is_some_and(|r| key_is(k, *r))
        {
            return;
        }
        if self.new_draft.is_some() {
            return self.on_draft_key(k);
        }
        if self.prompt.is_some() {
            return self.on_prompt_key(k);
        }
        if self.tool_menu.is_some() {
            return self.on_tool_key(k);
        }
        if !self.overlays.is_empty() {
            return self.on_overlay_key(k);
        }
        if self.space_menu.is_some() {
            return self.on_space_menu_key(k);
        }
        if self.group_menu.is_some() {
            return self.on_group_menu_key(k);
        }
        if self.only_menu.is_some() {
            return self.on_only_menu_key(k);
        }
        if self.theme_menu.is_some() {
            return self.on_theme_key(k);
        }
        if self.settings.is_some() {
            return self.on_settings_key(k);
        }
        // Confirm: only y counts, any other key (Enter included) cancels — delete can't be undone, restart interrupts it
        if let Some(c) = self.confirm.take() {
            let yes = k.code == KeyCode::Char('y');
            // Any key but y means no: Esc is an explicit no, nothing to say; other keys get a line, in case it was pressed by accident
            let no = |what: &'static str| if k.code == KeyCode::Esc { "" } else { what };
            let mut said = "";
            match c {
                Confirm::Remove(r) if yes => {
                    let t = Target { name: r.name, title: r.title, profile: r.profile, id: r.id };
                    // Remove from the list first, without waiting for the background delete (it waits for the process to exit cleanly)
                    let running = !t.id.is_empty()
                        && self.all.iter().any(|s| s.sub.is_none() && s.deck_id == t.id && s.profile == t.profile && !s.state.gone());
                    let job = Job::Remove { t: t.clone(), pane: r.pane.map(|(p, _)| p) };
                    let running = if running { vec![t.id] } else { vec![] };
                    self.vanish.push(Vanish { job: job.clone(), until: None, running });
                    self.job = Some(job);
                    self.drop_vanished();
                    self.apply_pins();
                    self.rebuild();
                }
                Confirm::Remove(r) if r.pane.is_some() => said = no(t!("未关闭", "not closed")),
                Confirm::Remove(_) => said = no(t!("未删除", "not deleted")),
                Confirm::Group(g) if yes => {
                    // Remove the group's sessions from the list first, without waiting for the background stop / delete
                    let job = Job::Group(groups::Act::Delete { profile: g.profile, group: g.group });
                    self.vanish.push(Vanish { job: job.clone(), until: None, running: g.running });
                    self.job = Some(job);
                    self.head = None;
                    self.drop_vanished();
                    self.apply_pins();
                    self.rebuild();
                }
                Confirm::Group(_) => said = no(t!("未删除", "not deleted")),
                Confirm::Restart { t, force, .. } if yes => self.job = Some(Job::Restart { t, force }),
                Confirm::Restart { .. } => said = no(t!("未重启", "not restarted")),
                Confirm::Stop { t, .. } if yes => self.job = Some(Job::Stop { t }),
                Confirm::Stop { .. } => said = no(t!("未停止", "not stopped")),
                Confirm::Pane { t, p, restart, .. } if yes => {
                    self.job = Some(if restart { Job::Respawn { t, p } } else { Job::Park { t, p } })
                }
                Confirm::Pane { restart, .. } => {
                    said = no(if restart { t!("未重启", "not restarted") } else { t!("未停止", "not stopped") })
                }
                Confirm::Send { t, pane, key, text } if yes => self.job = Some(Job::Send { t, pane, text, key }),
                Confirm::Send { .. } => said = no(t!("未发送", "not sent")),
                Confirm::Adopt { t, plan } if yes => self.job = Some(Job::Adopt { t, plan }),
                Confirm::Adopt { .. } => said = no(t!("未接管", "not adopted")),
                Confirm::Many { act, ts, .. } if yes => self.run_many(act, ts),
                Confirm::Many { act, .. } => {
                    said = no(match act {
                        Batch::Remove => t!("未删除", "not deleted"),
                        Batch::Stop => t!("未停止", "not stopped"),
                        Batch::Restart => t!("未重启", "not restarted"),
                    })
                }
            }
            if !said.is_empty() {
                self.flash(said);
            }
            return;
        }
        if self.bill.is_some() {
            return self.on_bill_key(k);
        }
        if self.trace.is_some() {
            return self.on_trace_key(k);
        }
        if self.reader.is_some() {
            return self.on_reader_key(k);
        }
        if self.filtering {
            match k.code {
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => self.filter.push(c),
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Enter => self.filtering = false,
                KeyCode::Esc => {
                    self.filter.clear();
                    self.filtering = false;
                }
                KeyCode::Up => return self.move_by(-1),
                KeyCode::Down => return self.move_by(1),
                _ => return,
            }
            self.selected = None;
            self.head = None;
            self.list.select(Some(0));
            return self.rebuild();
        }
        if self.jump.is_some() && self.on_jump_key(k) {
            return;
        }
        if let Some(p) = self.pending.take() {
            return self.on_pending_key(p, k);
        }
        if self.picking() && self.on_pick_key(k) {
            return;
        }
        if self.head.is_none() && self.current().is_some_and(|s| s.subagent.is_some()) && self.on_agent_row_key(k) {
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        // The reference key: pick a session, its reference goes into the highlighted one. After tmux's prefix key, another key does
        // nothing (as in tmux)
        if self.insert_to.is_none() && !self.ref_keys.is_empty() {
            if key_is(k, self.ref_keys[self.ref_got]) {
                self.ref_got += 1;
                if self.ref_got == self.ref_keys.len() {
                    self.ref_got = 0;
                    self.start_insert();
                }
                return;
            }
            if std::mem::take(&mut self.ref_got) > 0 {
                return;
            }
        }
        // Ctrl+letter means only what's listed below (^Z ^S ^G ^L ^N ^R ^E); any other does nothing rather than act as the bare
        // letter: tmux's prefix key Ctrl+B pressed here, where no tmux is in between (the interface opened straight in the terminal,
        // or in a tmux popup), would open the trash
        if ctrl && matches!(k.code, KeyCode::Char(c) if !"zsglnre".contains(c)) {
            return;
        }
        // Picking for `aca ui --ref`: only the keys for finding a session and taking it. The key that opened it, held a moment, repeats
        // into the list as it appears (b, which would open the trash), and nothing here should change a session
        if self.insert_to.is_some() {
            let pick = matches!(
                k.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::Esc
                    | KeyCode::Tab
                    | KeyCode::BackTab
                    | KeyCode::Char('j' | 'k' | 'h' | 'l' | 'J' | 'K' | 'g' | 'G' | '/' | ' ' | 'a' | 'W' | '{' | '}' | 'i' | 'q')
                    | KeyCode::Char('!' | '@' | '#' | '%' | '&' | '^')
            );
            match k.code {
                KeyCode::Enter => return self.enter(),
                // Started here: Esc / q stop picking, back where it was (a search typed is cleared first)
                KeyCode::Esc | KeyCode::Char('q') if self.insert_back.is_some() && self.filter.is_empty() => return self.end_insert(),
                _ if !pick || ctrl => return,
                _ => {}
            }
        }
        match k.code {
            KeyCode::Char(' ') if !self.view.is_empty() => self.jump = Some(String::new()),
            KeyCode::Char('z') if ctrl => self.undo_last(),
            KeyCode::Char('z') => self.open_reader(),
            KeyCode::Char('x') => self.open_diff(),
            KeyCode::Char('Z') => self.open_trace(),
            KeyCode::Char('t') => self.open_term(),
            KeyCode::Char('s') if ctrl => self.open_settings(),
            KeyCode::Char('s') => self.ask_send(),
            KeyCode::Char('T') => self.window_here(),
            KeyCode::Char('y') => self.open_yazi(),
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.rebuild();
            }
            KeyCode::Esc if let Some(p) = self.status => self.toggle_status(p),
            // In a tmux popup or while following, Esc closes (following first goes back to where it was when opened; `follow_step` in the
            // run loop switches). In a standalone terminal Esc at the bottom doesn't quit: every popup and page closes with Esc, one press
            // too many shouldn't close the whole interface; quitting is q q
            KeyCode::Esc if self.following.is_some() || crate::data::in_popup() => {
                if let Some(f) = self.following.as_mut() {
                    f.back();
                }
                self.quit = true
            }
            KeyCode::Esc => self.flash(t!("q q 退出", "q q quits")),
            KeyCode::Up if k.modifiers.contains(KeyModifiers::SHIFT) => self.reorder(-1),
            KeyCode::Down if k.modifiers.contains(KeyModifiers::SHIFT) => self.reorder(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Char('J') => self.jump_section(1),
            KeyCode::Char('K') => self.jump_section(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::PageUp => self.move_by(-5),
            KeyCode::PageDown => self.move_by(5),
            KeyCode::Char('g') if ctrl => self.note_in_editor(),
            KeyCode::Home | KeyCode::Char('g') => self.move_by(-10_000),
            KeyCode::End | KeyCode::Char('G') => self.move_by(10_000),
            KeyCode::Enter if k.modifiers.contains(KeyModifiers::SHIFT) => self.enter_with_note(),
            KeyCode::Enter => self.enter(),
            KeyCode::Char('i') => self.jump_waiting(),
            KeyCode::Char('/') => self.filtering = true,
            KeyCode::Char('a') => self.toggle_all(),
            KeyCode::Char('w') => self.toggle_open(),
            KeyCode::Char('!') => self.toggle_status(Pick::Run),
            KeyCode::Char('@') => self.toggle_status(Pick::Wait),
            KeyCode::Char('#') => self.toggle_status(Pick::Idle),
            KeyCode::Char('%') => self.toggle_status(Pick::Reaped),
            KeyCode::Char('&') => self.toggle_status(Pick::Err),
            KeyCode::Char('^') => self.toggle_status(Pick::Off),
            KeyCode::Tab => self.open_group_menu(),
            KeyCode::BackTab => self.back_group(),
            KeyCode::Char('}') => self.cycle_group(1),
            KeyCode::Char('{') => self.cycle_group(-1),
            KeyCode::Char('l') if ctrl => self.ask_relaunch(),
            KeyCode::Left | KeyCode::Char('h') => self.step_out(),
            KeyCode::Right | KeyCode::Char('l') => self.step_in(),
            KeyCode::Char('o') => self.toggle_fold(),
            KeyCode::Char('O') => self.toggle_fold_all(),
            KeyCode::Char('N') => self.ask_new(),
            KeyCode::Char('n') if ctrl => self.ask_new_group(),
            KeyCode::Char('n') => self.open_tools(),
            KeyCode::Char('e') if ctrl => self.open_special(),
            KeyCode::Char('d') if self.on_group_row() => self.ask_remove_group(),
            KeyCode::Char('d') => self.ask_remove(),
            KeyCode::Char('b') => self.open_trash(),
            KeyCode::Char('p') if self.on_group_row() => {
                self.on_group(t!("置顶", "pin"));
            }
            KeyCode::Char('p') => self.toggle_pin(),
            KeyCode::Char('u') => self.toggle_read(),
            KeyCode::Char('q') => self.pending = Some(Pending::Switch),
            KeyCode::Char('Q') if self.current().is_some() => self.pending = Some(Pending::Send),
            KeyCode::Char('[') => self.cycle_space(-1),
            KeyCode::Char(']') => self.cycle_space(1),
            KeyCode::Char('`') => self.back_space(),
            KeyCode::Char('S') => self.open_space_menu(),
            KeyCode::Char('c') => self.ask_copy(true),
            KeyCode::Char('B') => self.ask_copy(false),
            KeyCode::Char('C') => self.open_copy_menu(),
            KeyCode::Char(c @ ('v' | 'V')) => self.start_pick(c == 'V'),
            KeyCode::Char('L') => self.cycle_preview(),
            KeyCode::Char('r') if ctrl => self.refresh = true,
            KeyCode::Char('r') => self.ask_rename(),
            KeyCode::Char('R') => self.ask_restart(),
            KeyCode::Char('D') => self.ask_stop(),
            KeyCode::Char('m') => self.ask_move(),
            // F used to be "name it, then fork"; f does that now
            KeyCode::Char('f') => self.ask_fork(),
            KeyCode::Char('F') => self.open_only_menu(),
            KeyCode::Char('H') => self.ask_handoff(),
            KeyCode::Char('X') => self.ask_rehome(),
            KeyCode::Char('A') => self.ask_adopt(),
            KeyCode::Char('M') => self.open_mcp(),
            KeyCode::Char('P') => self.open_plugins(),
            KeyCode::Char(':') => self.open_caps(),
            KeyCode::Char('W') => self.toggle_own_group(),
            KeyCode::Char('*') => self.cycle_days(),
            KeyCode::Char('?') => self.open_help(),
            KeyCode::Char('$') => self.open_bill(),
            KeyCode::Char('e') => self.ask_note(),
            KeyCode::Char('E') => self.enter_with_note(),
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if self.digits_at.elapsed() > DIGIT_GAP {
                    self.digits.clear();
                }
                self.digits_at = Instant::now();
                self.digits.push(c);
                let n: usize = self.digits.parse().unwrap_or(0);
                if self.numbered(n).is_none() {
                    // No number 12 → treat it as pressing 2 afresh
                    self.digits = c.to_string();
                }
                let n: usize = self.digits.parse().unwrap_or(0);
                if let Some(p) = self.numbered(n) {
                    self.select(Some(p));
                }
            }
            _ => {}
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        if self.term_on.is_some() || self.theme_menu.is_some() || self.settings.is_some() {
            return;
        }
        if let Some(b) = self.bill.as_mut() {
            match m.kind {
                MouseEventKind::ScrollUp => b.scroll = b.scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => b.scroll = b.scroll.saturating_add(3),
                _ => {}
            }
            return;
        }
        if let Some(r) = self.reader.as_mut() {
            match m.kind {
                MouseEventKind::ScrollUp => r.scroll = r.scroll.saturating_add(3),
                MouseEventKind::ScrollDown => r.scroll = r.scroll.saturating_sub(3),
                _ => {}
            }
            return;
        }
        if let Some(v) = self.trace.as_mut() {
            let d = match m.kind {
                MouseEventKind::ScrollUp => -3,
                MouseEventKind::ScrollDown => 3,
                _ => return,
            };
            match v.detail.as_mut() {
                Some(x) => x.scroll = x.scroll.saturating_add_signed(d),
                // Two panes: scroll whichever side the mouse is on
                None if v.wide && m.column >= v.split => v.steps.scroll(d),
                None if v.wide => v.chat.scroll(d),
                None => v.cur().scroll(d),
            }
            return;
        }
        if matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) {
            if self.modal_open() {
                return;
            }
            let pos = ratatui::layout::Position::new(m.column, m.row);
            if self.list_area.contains(pos) {
                self.move_by(if m.kind == MouseEventKind::ScrollUp { -1 } else { 1 });
            } else if self.preview_area.contains(pos) && !self.on_group_row() && self.current().is_some_and(|s| s.subagent.is_none()) {
                // The preview scrolls back through what doesn't fit on this screen;
                // past the top, earlier stuff is in the full-screen reader
                match m.kind {
                    MouseEventKind::ScrollUp if self.preview_up >= self.preview_max => self.flash(t!("z 全屏阅读", "z read output")),
                    MouseEventKind::ScrollUp => self.preview_up = (self.preview_up + 3).min(self.preview_max),
                    _ => self.preview_up = self.preview_up.saturating_sub(3),
                }
            }
            return;
        }
        match m.kind {
            // Group list open: a click anywhere closes it
            MouseEventKind::Down(MouseButton::Left) if self.group_menu.is_some() => self.group_menu = None,
            MouseEventKind::Down(MouseButton::Left)
                if self.chip_area.contains(ratatui::layout::Position::new(m.column, m.row)) && !self.modal_open() =>
            {
                self.open_group_menu()
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let a = self.list_area;
                if m.column < a.x || m.column >= a.x + a.width || m.row < a.y || m.row >= a.y + a.height {
                    return;
                }
                // List items are one or two lines (two with a question), with section headers in between; add up heights from the current
                // scroll position to find the clicked one
                let mut y = a.y;
                for &(pos, head, h) in self.drawn.iter().skip(self.disp.offset()) {
                    if m.row < y + h {
                        let on = self.list.selected();
                        // Clicking the already highlighted row = Enter (go in / fold); group headers are clickable too, blank rows and the
                        // pinned block's header aren't
                        match (pos, head) {
                            (Some(p), _) if on == Some(p) && self.head.is_none() => self.enter(),
                            (Some(p), _) => self.select(Some(p)),
                            (None, Some(p)) if on == Some(p) && self.head.is_some() => self.enter(),
                            (None, Some(p)) => self.select_head(p),
                            (None, None) => {}
                        }
                        return;
                    }
                    y += h;
                }
            }
            _ => {}
        }
    }
}
