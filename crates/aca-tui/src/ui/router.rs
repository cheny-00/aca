//! A group's router on screen: which session it is, and — with the cursor on the group's row — its thread in the column
//! beside the list: what was said to it, what it answered, what it sent out and pulled over (see `aca_core::thread`).
//!
//! The scratch group's router is the desk: it dispatches for every group, so the rail's row for all groups stands for it —
//! its @ leads that row, its thread shows beside it, and `o` / `s` there reach it.

use aca_core::thread::{Entry, Kind};

use super::*;

/// The key a router's thread is marked seen under in the read file, next to the sessions' own marks
fn seen_key(id: &str) -> String {
    format!("{THREAD_KEY}{id}")
}

/// What leads a thread's key in the read file
pub(super) const THREAD_KEY: &str = "thread:";

/// Whether a group can have a router: the sessions in no group and the plain tmux sessions' section can't
pub(super) fn can_route(g: &str) -> bool {
    !g.is_empty() && g != "-"
}

impl App {
    /// Where `all` has the session group `g` names as its router, whatever its state and whether or not the list shows it
    pub(super) fn router_at(&self, g: &str) -> Option<usize> {
        self.all.iter().position(|s| s.router && s.sub.is_none() && s.subagent.is_none() && s.group == g)
    }

    pub(super) fn router_of(&self, g: &str) -> Option<&Session> {
        self.router_at(g).map(|i| &self.all[i])
    }

    /// The desk: the scratch group's router, which dispatches for every group. None while there's no scratch group, or it
    /// has no router yet
    pub(super) fn desk(&self) -> Option<&Session> {
        self.scratch.as_deref().and_then(|g| self.router_of(g))
    }

    /// The cursor is on the rail's row for all groups
    pub(super) fn on_all_row(&self) -> bool {
        self.rail == Some(RailAt::Here) && self.group.is_none()
    }

    /// The group whose router the row under the cursor stands for: a group's own on its row (see `group_row`); on the rail's
    /// row for all groups the scratch group, whose router is the desk. None = on a session or the Trash, or on the row for
    /// all groups with no scratch group set up
    pub(super) fn route_group(&self) -> Option<&str> {
        if self.on_all_row() { self.scratch.as_deref() } else { self.group_row() }
    }

    /// The cursor is on a row `o` and `s` work from: a group's, or the rail's row for all groups
    pub(super) fn on_route_row(&self) -> bool {
        self.on_group_row() || self.on_all_row()
    }

    /// Whether group `g` is the scratch group: its router is the desk, and is called that
    pub(super) fn is_scratch(&self, g: &str) -> bool {
        self.scratch.as_deref() == Some(g)
    }

    /// A group's name as written on screen: one under the scratch group has the scratch mark for that part of its path
    pub(super) fn shown<'a>(&self, g: &'a str) -> std::borrow::Cow<'a, str> {
        group_shown(g, scratch_len(g, self.scratch.as_deref()))
    }

    /// o: into the router of the group the row under the cursor belongs to (one that isn't running is started first, as ⏎ on
    /// its own row does), what it has said read up to now; back from it the cursor is still on that row. On a group's row it
    /// is that group's, on a session's row the one of the group the session is in. The scratch group's router is the desk,
    /// and so is the one of the rail's row for all groups. A group with none: the new-session box, for a router (with no
    /// scratch group for the desk to be in yet, that group's directory is asked for first)
    pub(super) fn open_router(&mut self) {
        self.digits.clear();
        let row = self.on_route_row();
        let group = match row {
            true => self.route_group().map(String::from),
            // The Trash stands for no group
            false if self.on_rail() => return,
            // A group's tasks or notes page: that group's
            false if self.off_list() => self.group.clone(),
            false => self.current().map(|s| s.group.clone()),
        };
        let Some(g) = group.filter(|g| can_route(g)) else {
            if self.on_all_row() {
                // A directory is set but no group works in it any more: it's offered again, ⏎ makes the group
                let text = words::tilde(&(self.scratch_dir)());
                self.prompt = Some(Prompt { ask: Ask::ScratchDir { desk: true }, text, hint: String::new() });
            } else if !row && self.current().is_some() {
                // A plain tmux session, one in no group
                self.flash(t!("不在分组里,没有 router", "in no group: no router"));
            }
            return;
        };
        if let Some(i) = self.router_at(&g) {
            self.stay = Some(self.all[i].name.clone());
            // What it has said so far is about to be read where it said it
            match row {
                true => self.thread_read(),
                false => {
                    let seen = seen_key(&self.all[i].deck_id);
                    let _ = self.reads.mark_read(&seen, epoch(), &self.keep_marks());
                }
            }
            return self.go(i);
        }
        if self.is_scratch(&g) {
            self.new_draft = Some(self.desk_draft(&g, &(self.scratch_dir)()));
            return;
        }
        // From a session's row: the tool, account and directory N on its group's row would start with
        let draft = match (row, self.group_dir(&g)) {
            (false, Some((s, dir, _))) => Ok(Draft::in_group(s, &g, &dir, self.group_hints)),
            _ => self.draft(),
        };
        match draft {
            Ok(d) => {
                self.new_draft = Some(d.for_router(&g));
                self.pick = self.pick_first();
            }
            Err(e) => self.flash(&e),
        }
    }

    /// The new-session box for the desk: in the scratch group `g` and its directory `dir`, with the tool and account N on
    /// that group's row would start with: those of its latest session. Nothing was started there yet: those of the row the
    /// list is on, as N on the row for all groups
    pub(super) fn desk_draft(&self, g: &str, dir: &str) -> Draft {
        let latest = self.group_dir(g).map(|(s, ..)| s).filter(|s| !s.blank);
        let like = latest.or_else(|| self.current()).filter(|s| !s.profile.is_empty());
        Draft::in_scratch(like, g, dir, self.group_hints).for_desk(g)
    }

    /// A session was just made (the run loop, before going into it). A router: back from it the cursor stays on the row o
    /// was pressed on; an opening message that didn't reach it is kept as its draft, where `s` sends it
    pub fn created(&mut self, req: &NewReq, made: &create::Created) {
        if !req.router {
            return;
        }
        self.stay = Some(made.id.clone());
        if let Some((text, why)) = &made.unsaid {
            self.save_note(&made.id, text);
            self.flash(&tr!("开场话未送达:{why} · 已存入便签,s 回话发出", "its brief wasn't sent: {why} · the draft has it, s replies with it"));
        }
    }

    /// The thread showing has been read up to now: nothing in it is news
    pub(super) fn thread_read(&mut self) {
        let Some((id, seen)) = self.thread_on.as_mut() else { return };
        *seen = epoch();
        let key = seen_key(id);
        let _ = self.reads.mark_read(&key, epoch(), &self.keep_marks());
    }

    /// The groups that have a router, each with the color of its router's state (for the @ before the group's name). Not
    /// the scratch group: its router is the desk, whose @ leads the rail's row for all groups instead
    pub(super) fn group_routers(&self) -> Vec<(String, Style)> {
        let routers = self.all.iter().filter(|s| s.router && !s.desk && s.sub.is_none() && s.subagent.is_none());
        routers.map(|s| (s.group.clone(), session_icon(s).1)).collect()
    }

    /// The router the row under the cursor stands for: its group's on a group's row (on the rail, or its header in the
    /// list), the desk on the rail's row for all groups
    pub(super) fn row_router(&self) -> Option<&Session> {
        self.route_group().and_then(|g| self.router_of(g))
    }

    /// Which router's thread is showing, and up to when it had been seen on getting there: entries later than that are new
    /// to you (bold) for as long as the cursor stays. Moving off marks it seen up to now. Called as the screen is drawn
    pub(super) fn follow_thread(&mut self) -> Option<u64> {
        let now_on = self.row_router().map(|r| r.deck_id.clone());
        if self.thread_on.as_ref().map(|t| &t.0) != now_on.as_ref() {
            if let Some((left, _)) = self.thread_on.take() {
                let _ = self.reads.mark_read(&seen_key(&left), epoch(), &self.keep_marks());
            }
            // Never looked at before: nothing in it is news yet
            self.thread_on = now_on.map(|id| {
                let seen = self.reads.read_at(&seen_key(&id)).unwrap_or(u64::MAX);
                (id, seen)
            });
        }
        self.thread_on.as_ref().map(|t| t.1)
    }
}

/// What a notice says happened, in the interface's words
fn notice_word(what: &str) -> &'static str {
    match what {
        "turn.done" => t!("跑完一轮", "finished a turn"),
        "turn.failed" => t!("出错了", "failed"),
        "agent.exited" => t!("退出了", "exited"),
        "agent.waiting" => t!("在等确认", "waiting"),
        _ => t!("有动静", "something happened"),
    }
}

/// A group as an entry names it: the last level of its path
fn short_group(g: &str) -> &str {
    let label = group_label(g);
    if label != g { label } else { g.rsplit('/').next().unwrap_or(g) }
}

/// Words cut into at most `rows` lines of `w` cells; what doesn't fit ends in …
fn wrap_rows(text: &str, w: usize, rows: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut used = 0;
    for ch in text.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > w {
            if out.len() == rows {
                let last = out.last_mut().unwrap();
                *last = clip(&format!("{last}{ch}"), w).trim_end().to_string();
                return out;
            }
            out.push(String::new());
            used = 0;
        }
        out.last_mut().unwrap().push(ch);
        used += cw;
    }
    out
}

/// One entry of a thread as lines `w` cells wide: a mark saying who and which way, the words, and at the right edge how long
/// ago (a reply has none: it follows what it answers), after the channel what you said came in through, when it did. `name`: a session's title by its id. `home`: the router's own group.
/// `fresh`: later than you last looked, so bold
fn entry_lines(e: &Entry, name: &dyn Fn(&str) -> String, home: &str, fresh: bool, now: u64, w: usize) -> Vec<Line<'static>> {
    let pal = theme::pal();
    let words = if fresh { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
    let mark = |t: &str| (t.to_string(), dim());
    let red = |t: &str| (t.to_string(), Style::new().fg(pal.red));
    let who = || (name(&e.who), words);
    if e.kind == Kind::Reply {
        let rows = wrap_rows(&e.text, w.saturating_sub(2), 2);
        return rows
            .into_iter()
            .enumerate()
            .map(|(i, r)| Line::from(vec![Span::styled(if i == 0 { "● " } else { "  " }, words), Span::styled(r, words)]))
            .collect();
    }
    let left: Vec<(String, Style)> = match e.kind {
        Kind::You => vec![mark("❯ "), (e.text.clone(), words)],
        Kind::From => vec![who(), mark(" ❯ "), (e.text.clone(), words)],
        Kind::Notice => vec![who(), mark(" · "), (notice_word(&e.text).to_string(), dim())],
        Kind::WorkerReply => vec![who(), mark(" ● "), (e.text.clone(), words)],
        Kind::WorkerAsks => vec![who(), red(" ◆ "), (e.text.clone(), words)],
        Kind::WorkerFailed => vec![who(), red(" ✕ "), (e.text.clone(), words)],
        Kind::TaskDone => vec![(tr!("#{} 完成", "#{} done", e.aux), words), mark(" · "), (e.text.clone(), words)],
        Kind::To => vec![mark("@ → "), who(), ("  ".into(), words), (e.text.clone(), words)],
        Kind::Failed => vec![red("✕ "), (e.text.clone(), words)],
        Kind::Asks => vec![red("◆ "), (e.text.clone(), words)],
        Kind::Pulled if e.aux == home => vec![mark("@ ← "), (short_group(&e.text).to_string(), dim()), ("  ".into(), words), who()],
        Kind::Pulled => vec![mark("@ "), who(), mark(" → "), (short_group(&e.aux).to_string(), dim())],
        Kind::Started => vec![mark("@ → "), who(), ("  ".into(), words), (t!("新会话", "new session").to_string(), dim())],
        Kind::Reply => vec![],
    };
    // At the right edge: a message still on its way says so, then the channel it came in through, then how long ago
    let age = crate::data::age(now.saturating_sub(e.ts));
    let waits = e.pending && matches!(e.kind, Kind::You | Kind::From | Kind::To);
    let tag = if waits { format!("{}  ", aca_core::inbox::Stage::Pending.said()) } else { String::new() };
    let age = if e.via.is_empty() { age } else { format!("{}  {age}", e.via) };
    let right_w = tag.width() + age.width();
    let room = w.saturating_sub(right_w + 2);
    let mut spans: Vec<Span<'static>> = vec![];
    let mut used = 0;
    for (text, st) in left {
        if used >= room {
            break;
        }
        let text = if used + text.width() > room { clip(&text, room - used).trim_end().to_string() } else { text };
        used += text.width();
        spans.push(Span::styled(text, st));
    }
    spans.push(Span::raw(" ".repeat(w.saturating_sub(used + right_w))));
    spans.push(Span::styled(tag, theme::warn()));
    spans.push(Span::styled(age, dim()));
    vec![Line::from(spans)]
}

/// How a project stands, as its row on the rail writes it: its tickets done of all it has (None: it has none), and how many
/// of its sessions are in each of the rail's marks
pub(super) type Standing = (Option<(usize, usize)>, [usize; 4]);

/// What the column beside the list shows for a group that has a router
pub(super) struct RouterPane<'a> {
    pub router: &'a Session,
    /// The group's directory, and its branch when it's a repository
    pub dir: Option<&'a str>,
    pub branch: Option<&'a str>,
    /// The notes pinned in the group's notebook
    pub pins: &'a [list::Pin<'a>],
    /// A project's router: how the project stands, said on the line under the title in place of the router's own state
    /// (the mark before the title says that). None for the desk, whose line keeps its state
    pub tally: Option<Standing>,
    /// Up to when you had seen its thread
    pub seen: u64,
    pub now: u64,
    /// A reply to it being written: the words so far
    pub writing: Option<&'a str>,
    /// Its draft: a reply written and not sent
    pub kept: Option<&'a str>,
}

/// How many lines of a reply being written show: the last ones, where the cursor is
const PEN_ROWS: usize = 4;

/// The line a reply goes on, at the bottom of the column, always there: it is where s writes. Not being written: what was
/// left unsent, if anything. The screen lays it on the sunk ground, a band across the column (see `draw_screen`)
pub(super) fn pen_lines(p: &RouterPane, w: usize) -> Vec<Line<'static>> {
    match (p.writing, p.kept.filter(|k| !k.trim().is_empty())) {
        (Some(text), _) => {
            let rows: Vec<String> = format!("{text}▏").split('\n').flat_map(|l| wrap_width(l, w.saturating_sub(2))).collect();
            let skip = rows.len().saturating_sub(PEN_ROWS);
            rows.into_iter()
                .skip(skip)
                .enumerate()
                .map(|(i, row)| Line::from(vec![Span::styled(if i == 0 { "❯ " } else { "  " }, theme::accent()), Span::raw(row)]))
                .collect()
        }
        (None, Some(kept)) => {
            let first = kept.lines().next().unwrap_or_default();
            vec![Line::styled(format!("❯ {}", clip(first, w.saturating_sub(2)).trim_end()), dim())]
        }
        (None, None) if list::wakes_on_send(p.router) => vec![Line::styled(t!("❯ s 发话唤醒", "❯ s sends and wakes"), dim())],
        (None, None) => vec![Line::styled("❯", dim())],
    }
}

/// The column, `w` by `h`: the router's state and where the group works; what's pinned and what it has scheduled; then the
/// thread, the latest of it when there's more than fits; and at the bottom the line a reply is written on.
/// `name`: a session's title by its id
pub(super) fn router_pane(p: &RouterPane, name: &dyn Fn(&str) -> String, w: usize, h: usize) -> Vec<Line<'static>> {
    let r = p.router;
    let mut head: Vec<(String, Style)> = match p.tally {
        None => vec![(state_word(r), dim())],
        Some((tasks, marks)) => {
            let mut v: Vec<(String, Style)> = vec![];
            if let Some((done, all)) = tasks {
                v.push((format!("{done}/{all}"), Style::new().add_modifier(Modifier::BOLD)));
            }
            let marks = super::rail::mark_spans(&marks);
            if !marks.is_empty() && !v.is_empty() {
                v.push(("  ".into(), Style::new()));
            }
            v.extend(marks.into_iter().map(|s| (s.content.into_owned(), s.style)));
            v
        }
    };
    if let Some(dir) = p.dir {
        let branch = p.branch.filter(|b| !b.is_empty()).map(|b| format!("  {b}")).unwrap_or_default();
        let lead: usize = head.iter().map(|(t, _)| t.width()).sum();
        let gap = if lead > 0 { "   " } else { "" };
        let room = w.saturating_sub(lead + gap.len() + branch.width());
        if room >= 8 {
            head.extend([(gap.into(), Style::new()), (clip_left(&words::tilde(dir), room), Style::new()), (branch, dim())]);
        }
    }
    let mut top = vec![list::fit_line(head, w)];
    top.extend(pinned_lines(p.pins, w, (h / 4).max(1)));
    for d in r.due.iter().take(2) {
        top.push(Line::styled(clip(&list::due_line(d, p.now), w).trim_end().to_string(), dim()));
    }
    let pen = pen_lines(p, w);
    let room = h.saturating_sub(top.len() + 1 + pen.len() + 1);
    // The thread: a blank line before what someone says to it, and before anything that comes a while after the last
    let home = r.group.as_str();
    let mut body: Vec<Line<'static>> = vec![];
    let mut last: Option<&Entry> = None;
    for e in &r.thread {
        let said_to_it = matches!(e.kind, Kind::You | Kind::From);
        if last.is_some_and(|l| said_to_it || e.ts > l.ts + 60) {
            body.push(Line::default());
        }
        let fresh = e.ts > p.seen && e.kind != Kind::You;
        body.extend(entry_lines(e, name, home, fresh, p.now, w));
        last = Some(e);
    }
    let mut skip = body.len().saturating_sub(room);
    // Don't start on a blank line
    while body.get(skip).is_some_and(|l| l.spans.is_empty()) {
        skip += 1;
    }
    let body: Vec<Line<'static>> = body.into_iter().skip(skip).collect();
    let mut out = top;
    out.push(Line::default());
    let gap = h.saturating_sub(out.len() + body.len() + pen.len());
    out.extend(body);
    out.extend(std::iter::repeat_n(Line::default(), gap));
    out.extend(pen);
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::*;
    use super::*;

    fn entry(ts: u64, kind: Kind, who: &str, text: &str) -> Entry {
        Entry { ts, kind, who: who.into(), text: text.into(), aux: String::new(), id: String::new(), pending: false, via: String::new() }
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().trim_end().to_string()).collect()
    }

    fn names(id: &str) -> String {
        match id {
            "a1" => "NUC 待机功耗".into(),
            "b2" => "fix-1117-panel".into(),
            other => other.to_string(),
        }
    }

    /// A router with a thread: you said something, it pulled two sessions over and answered; it handed one of them
    /// something that hasn't arrived; that one asked back
    fn router() -> Session {
        let mut r = mk("nuc router", "nuc", State::Running, 0);
        r.router = true;
        r.ran = 60;
        r.thread = vec![
            entry(940, Kind::You, "", "随手问里查过 NUC 的电压和待机功耗，拉过来接着查"),
            Entry { aux: "nuc".into(), ..entry(941, Kind::Pulled, "a1", "随手问") },
            Entry { aux: "nuc".into(), ..entry(941, Kind::Pulled, "gone", "home/随手问") },
            entry(942, Kind::Reply, "", "拉过来 2 个。"),
            Entry { pending: true, id: "m1".into(), ..entry(1010, Kind::To, "b2", "放 shots/，周五要发") },
            entry(1055, Kind::From, "b2", "截图放哪个目录？"),
        ];
        r
    }

    /// Lines with their runs of spaces squeezed to one: what is on each, whatever the column's width
    fn squeezed(lines: &[String]) -> Vec<String> {
        lines.iter().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect()
    }

    #[test]
    fn the_column_is_the_routers_state_then_whats_pinned_then_its_thread_and_the_line_to_reply_on() {
        let r = router();
        let pins = [("发版窗口", "周二、周四别上集群")];
        let p = RouterPane {
            router: &r,
            dir: Some("/p/nuc"),
            branch: Some("main"),
            pins: &pins,
            tally: None,
            seen: u64::MAX,
            now: 1060,
            writing: None,
            kept: None,
        };
        let got = text(&router_pane(&p, &names, 60, 16));
        assert_eq!(
            squeezed(&got),
            [
                "运行中 · 1 分钟 /p/nuc main",
                // A pinned note: its name, then the start of what it says
                "★ 发版窗口 周二、周四别上集群",
                "",
                "❯ 随手问里查过 NUC 的电压和待机功耗，拉过来接着查 2分",
                "@ ← 随手问 NUC 待机功耗 1分",
                // A session that is gone goes by its id; a nested group by its last level
                "@ ← 随手问 gone 1分",
                "● 拉过来 2 个。",
                // What comes a while after the last thing, and what someone says to it, has a blank line before it
                "",
                "@ → fix-1117-panel 放 shots/，周五要发 未送达 50秒",
                "",
                "fix-1117-panel ❯ 截图放哪个目录？ 5秒",
                "",
                "",
                "",
                "",
                "❯",
            ]
        );
        // How long ago ends at the column's right edge on every line that has it
        let aged: Vec<&String> = got.iter().filter(|l| l.ends_with('分') || l.ends_with('秒')).collect();
        assert_eq!(aged.len(), 5);
        assert!(aged.iter().all(|l| l.width() == 60), "{got:?}");
        assert!(got.iter().all(|l| l.width() <= 60), "{got:?}");
        // A project's router (not the desk): that line says how the project stands, as its row on the rail does — tickets
        // done of all, then its sessions by mark (◆ alone for one, ●2 for two) — and the router's own state is left to the
        // mark before the title
        let p = RouterPane { tally: Some((Some((6, 11)), [1, 2, 0, 0])), ..p };
        let lines = router_pane(&p, &names, 60, 16);
        assert_eq!(squeezed(&text(&lines))[0], "6/11 ◆ ●2 /p/nuc main");
        assert!(lines[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
        // No tickets and none of its sessions in a mark: only where it works
        let p = RouterPane { tally: Some((None, [0; 4])), ..p };
        assert_eq!(text(&router_pane(&p, &names, 60, 16))[0].trim_end(), "/p/nuc  main");
    }

    /// The fixture with 「跑」 as the router of its group (work), and a short exchange with it
    fn with_router() -> App {
        let mut v = sessions();
        let r = v.iter_mut().find(|s| s.title == "跑").unwrap();
        r.router = true;
        r.thread = vec![entry(999_000, Kind::You, "", "先看登录"), entry(999_100, Kind::Reply, "", "交给「闲」了。")];
        App::new(String::new(), v)
    }

    /// The line a reply goes on, at the bottom of a router's column, lies on the sunk ground: a band a cell wider than the
    /// words either side, the column's own ground above it and at its edges
    #[test]
    fn the_line_a_reply_goes_on_is_a_band_of_the_sunk_ground_across_the_column() {
        let mut app = with_router();
        keys(&mut app, "hjj");
        let mut t = Terminal::new(TestBackend::new(150, 40)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer();
        let pa = app.preview_area;
        // (what you said in the thread starts with ❯ too: the last one is the line's)
        let y = (pa.y..pa.bottom()).rev().find(|&y| buf[(pa.x + 2, y)].symbol() == "❯").expect("❯");
        assert_eq!(y, pa.bottom() - 1);
        assert!((pa.x + 1..pa.right() - 1).all(|x| buf[(x, y)].bg == theme::pal().back));
        let raised = [buf[(pa.x, y)].bg, buf[(pa.right() - 1, y)].bg, buf[(pa.x + 2, y - 1)].bg];
        assert_eq!(raised, [Color::Reset; 3]);
    }

    #[test]
    fn a_group_with_a_router_leads_with_its_mark_on_the_rail_its_header_and_the_routers_own_row() {
        let mut app = with_router();
        let rows = reader_rows(&mut app, 150, 24);
        let rail = |g: &str| rows.iter().map(|r| r.chars().take(28).collect::<String>()).find(|r| r.contains(g)).unwrap();
        // On the rail: before the name, where a group without one has nothing
        assert!(rail("work").contains("@ work") && !rail("fun").contains('@'), "{rows:#?}");
        // Its header in the list, and the router's own row in place of its state's mark
        let list: Vec<String> = rows.iter().map(|r| r.chars().skip(29).collect()).collect();
        assert!(list.iter().any(|r| r.trim_start().starts_with("@ work")), "{list:#?}");
        assert!(list.iter().any(|r| r.contains("@ 跑")) && list.iter().any(|r| r.contains("○ 闲")), "{list:#?}");
        assert_eq!(app.group_routers().len(), 1);
        assert_eq!(app.router_of("work").map(|s| s.title.as_str()), Some("跑"));
        assert!(app.router_of("fun").is_none());
        // A narrower screen has no rail: the header in the list still says it
        let narrow = reader_rows(&mut app, 100, 24);
        assert!(narrow.iter().any(|r| r.trim_start().starts_with("@ work")), "{narrow:#?}");
        // The letters showing: a top-level group's letter takes the mark's two cells for as long as they show
        press(&mut app, KeyCode::Char(' '));
        let rows = reader_rows(&mut app, 150, 24);
        assert!(rows.iter().any(|r| r.chars().take(28).collect::<String>().contains("W work")), "{rows:#?}");
    }

    #[test]
    fn on_the_groups_row_the_column_beside_is_the_routers_thread_and_leaving_marks_it_seen() {
        let mut app = with_router();
        keys(&mut app, "hjj");
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("work"), true));
        let rows = reader_rows(&mut app, 150, 24);
        // A line of the column that holds nothing but the mark a reply is written after
        let pen = |rows: &[String]| rows.iter().any(|r| r.split('│').any(|cell| cell.trim() == "❯"));
        // The head is the router's own (its state's mark, its name), then the exchange, and the line to reply on at the bottom
        assert!(rows[0].contains("● 跑"), "{rows:#?}");
        shows(&rows, &["❯ 先看登录", "● 交给「闲」了。"]);
        assert!(pen(&rows[rows.len() - 4..]), "{rows:#?}");
        // Nothing was marked while it shows; moving to another row marks it seen up to now
        assert_eq!(app.thread_on.as_ref().map(|t| t.0.as_str()), Some("id-跑"));
        assert!(app.reads.read_at("thread:id-跑").is_none());
        press(&mut app, KeyCode::Char('k'));
        reader_rows(&mut app, 150, 24);
        assert!(app.thread_on.is_none() && app.reads.read_at("thread:id-跑").is_some());
        // A group without a router: the column is the group's own, as before
        assert_eq!(app.group.as_deref(), Some("fun"));
        let rows = reader_rows(&mut app, 150, 24);
        assert!(rows[0].contains("▸ fun") && !pen(&rows), "{rows:#?}");
        // The mark of a thread stays in the read file as long as its session is listed
        let keep = app.keep_marks();
        assert!(keep("thread:id-跑") && keep("id-跑") && !keep("thread:id-nobody"));
    }

    /// The bottom bar of a 150×40 frame
    fn bar(app: &mut App) -> String {
        reader_rows(app, 150, 40).into_iter().rev().find(|r| !r.trim().is_empty()).unwrap_or_default()
    }

    #[test]
    fn o_on_a_groups_row_goes_into_its_router_and_back_from_it_the_cursor_is_still_on_that_row() {
        let mut app = with_router();
        keys(&mut app, "hjj");
        let b = bar(&mut app);
        assert!(b.contains(&caps("⏎ 进入 • s 回话 • o 打开 router • Tab 换页 • e 写笔记 • N 在组里新建 • r 改名")), "{b}");
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.attach.take().as_deref(), Some("跑"));
        // What it said so far is read in there: marked seen on the way in
        assert!(app.reads.read_at("thread:id-跑").is_some());
        app.came_back("跑", "$跑");
        assert_eq!((app.group.as_deref(), app.on_rail(), app.stay.is_none()), (Some("work"), true, true));
        // Another session switched to in there: the cursor goes to that one, as from any session
        press(&mut app, KeyCode::Char('o'));
        app.came_back("跑", "$闲");
        app.rebuild();
        assert_eq!((app.on_rail(), app.current().map(|s| s.title.as_str())), (false, Some("闲")));
        // Its header in the list is the group's row too
        let mut app = with_router();
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.attach.take().as_deref(), Some("跑"));
        app.came_back("跑", "");
        assert_eq!(app.head.as_deref(), Some("work"));
        // On a session's row it is the router of the session's group (see the next test)
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.attach.take().as_deref(), Some("跑"));
    }

    #[test]
    fn o_on_a_sessions_row_goes_into_the_router_of_its_group_and_back_from_it_the_cursor_is_on_that_session() {
        let mut app = with_router();
        select(&mut app, "错");
        press(&mut app, KeyCode::Char('o'));
        // Its thread read on the way in, as from the group's row
        assert_eq!(app.attach.take().as_deref(), Some("跑"));
        assert!(app.reads.read_at("thread:id-跑").is_some());
        app.came_back("跑", "$跑");
        app.rebuild();
        assert_eq!((app.on_rail(), app.group.as_deref(), app.current().map(|s| s.title.as_str())), (false, None, Some("错")));
        // The router's own row: itself
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.attach.take().as_deref(), Some("跑"));
        app.came_back("跑", "$跑");
        // A group with no router: the box for a new one in that group, the cursor staying on the session
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('o'));
        let d = app.new_draft.as_ref().unwrap();
        assert!(d.router && !d.desk && !app.pick);
        assert_eq!((app.on_rail(), app.current().map(|s| s.title.as_str())), (false, Some("问")));
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.group.as_str(), r.router), ("fun", true));
        // Made: back from it the cursor is still on that session
        app.created(&r, &create::Created { id: "r1".into(), unsaid: None });
        app.came_back("r1", "");
        assert_eq!((app.on_rail(), app.current().map(|s| s.title.as_str())), (false, Some("问")));
        // A plain tmux session is in no group: nothing to open, the bar says so
        let mut s = mk("外", "-", State::Idle, 5);
        (s.deck_id, s.profile) = (String::new(), String::new());
        let mut app = App::new(String::new(), vec![s]);
        press(&mut app, KeyCode::Char('o'));
        assert!(app.attach.is_none() && app.new_draft.is_none());
        assert_eq!(said(&app), "不在分组里,没有 router");
        // A session of the scratch group: its router is the desk, and ? calls it that
        let mut app = with_scratch(Some(State::Running));
        select(&mut app, "查电压");
        press(&mut app, KeyCode::Char('?'));
        shows(&reader_rows(&mut app, 150, 60), &["打开 / 新建总台"]);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.attach.take().as_deref(), Some("总台"));
        // No desk yet: the box for one
        let mut app = with_scratch(None);
        select(&mut app, "查电压");
        press(&mut app, KeyCode::Char('o'));
        assert!(app.new_draft.as_ref().is_some_and(|d| d.router && d.desk));
    }

    #[test]
    fn backtick_puts_the_cursor_on_the_rail_and_a_groups_letter_on_that_groups_row() {
        let mut app = with_router();
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('`'));
        // On the rail, on the row of where the list is (all groups), the letters showing
        assert_eq!((app.on_rail(), app.group.as_deref()), (true, None));
        let rail = rail_of(&reader_rows(&mut app, 150, 40));
        assert!(rail.iter().any(|r| r.contains("F fun")) && rail.iter().any(|r| r.contains("W work")), "{rail:#?}");
        assert!(bar(&mut app).contains(&caps("字母 到分组 • ⏎ 进入 • o router • ` 返回")), "{}", bar(&mut app));
        // A letter (no ⇧ needed: no session has one now): on that group's row, its router's thread beside; the letters go
        press(&mut app, KeyCode::Char('w'));
        assert_eq!((app.on_rail(), app.group.as_deref(), app.letters.is_none()), (true, Some("work"), true));
        shows(&reader_rows(&mut app, 150, 24), &["❯ 先看登录", "● 交给「闲」了。"]);
        // The row's own keys from there: o into its router, back on the row
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.attach.take().as_deref(), Some("跑"));
        app.came_back("跑", "$跑");
        assert_eq!((app.on_rail(), app.group.as_deref()), (true, Some("work")));
        // ⇧ and the letter too; ⏎ into the group's list
        keys(&mut app, "`F⏎");
        assert_eq!((app.on_rail(), app.group.as_deref()), (false, Some("fun")));
        // ` again or Esc while the letters show: back where it was, in the list
        for back in [KeyCode::Char('`'), KeyCode::Esc] {
            let mut app = with_router();
            select(&mut app, "问");
            press(&mut app, KeyCode::Char('`'));
            press(&mut app, back);
            assert_eq!((app.on_rail(), app.letters.is_none(), app.current().map(|s| s.title.as_str())), (false, true, Some("问")));
        }
        // Another key, a letter no group has too, acts on the rail: j moves along it
        let mut app = with_router();
        keys(&mut app, "`j");
        assert_eq!((app.on_rail(), app.group.as_deref(), app.letters.is_none()), (true, Some("fun"), true));
        // Inside a group, ` o is that group's router
        let mut app = with_router();
        keys(&mut app, " W`o");
        assert_eq!(app.attach.take().as_deref(), Some("跑"));
        // A screen with no rail: it is laid over the left; a letter, then Esc puts it away, the list back where it was
        let mut app = with_router();
        reader_rows(&mut app, 100, 24);
        press(&mut app, KeyCode::Char('`'));
        let rows = reader_rows(&mut app, 100, 24);
        assert!(rows.iter().any(|r| r.contains("W work")), "{rows:#?}");
        press(&mut app, KeyCode::Char('w'));
        let rows = reader_rows(&mut app, 100, 24);
        assert!(app.on_rail() && rows.iter().any(|r| r.contains("@ work")), "{rows:#?}");
        press(&mut app, KeyCode::Esc);
        assert_eq!((app.on_rail(), app.group.as_deref()), (false, None));
    }

    /// 「随手问」 works in /h; the other groups have no directory
    fn asks_hints() -> crate::data::GroupHints {
        let g = |n: &str, d: &str| (n.to_string(), d.to_string());
        crate::data::GroupHints { groups: vec![g("fun", ""), g("work", ""), g("随手问", "/h")] }
    }

    /// The fixture with /h as the scratch directory and a session in the group that works there. `desk`: that group has a
    /// router in this state, with a short exchange — the desk
    fn with_scratch(desk: Option<State>) -> App {
        let mut v = sessions();
        let mut add = |mut s: Session| {
            s.slot = (2, v.len() as i64, 0);
            v.push(s);
        };
        add(Session { tool: "codex".into(), agent: Agent::Codex, ..mk("查电压", "随手问", State::Idle, 20) });
        if let Some(state) = desk {
            let mut d = mk("总台", "随手问", state, 30);
            d.router = true;
            d.thread = vec![entry(999_000, Kind::You, "", "NUC 的事给谁"), entry(999_100, Kind::Reply, "", "交给「查电压」了。")];
            add(d);
        }
        App::new(String::new(), v).with_group_hints(asks_hints).with_scratch(|| "/h".into(), |_| Ok("随手问".into()))
    }

    /// The first 28 columns of the rows of a wide frame: the rail
    fn rail_of(rows: &[String]) -> Vec<String> {
        rows.iter().map(|r| r.chars().take(28).collect()).collect()
    }

    #[test]
    fn the_desk_leads_the_row_for_all_groups_and_its_thread_is_beside_that_row_and_the_scratch_groups() {
        let mut app = with_scratch(Some(State::Running));
        let rows = reader_rows(&mut app, 150, 24);
        let rail = rail_of(&rows);
        // Its @ is on the row for all groups and on its own row; the scratch group's name has none, on the rail or in the list
        assert!(rail.iter().any(|r| r.contains("@ 全部")), "{rail:#?}");
        assert!(rail.iter().any(|r| r.contains("⌂ 随手问")) && !rail.iter().any(|r| r.contains("@ 随手问")), "{rail:#?}");
        let list: Vec<String> = rows.iter().map(|r| r.chars().skip(29).collect()).collect();
        assert!(list.iter().any(|r| r.contains("@ 总台")) && !list.iter().any(|r| r.contains("@ 随手问")), "{list:#?}");
        assert!(app.group_routers().is_empty());
        assert_eq!(app.desk().map(|s| s.title.as_str()), Some("总台"));
        // `aca ui -l` goes by the same rows
        let view: Vec<&Session> = app.view.iter().map(|&i| &app.all[i]).collect();
        let printed: Vec<String> =
            list_rows(&view, true, 60, None, 0, false).iter().map(|r| r.lines[0].iter().map(|(t, _)| t.as_str()).collect()).collect();
        assert!(printed.iter().any(|r| r.contains("⌂ 随手问")) && printed.iter().any(|r| r.contains("@ 总台")), "{printed:#?}");
        // On the row for all groups: the column beside is the desk's, its state and where the scratch group works at the top
        press(&mut app, KeyCode::Char('h'));
        assert_eq!((app.group.as_deref(), app.on_rail()), (None, true));
        let rows = reader_rows(&mut app, 150, 24);
        assert!(rows[0].contains("● 总台"), "{rows:#?}");
        shows(&rows, &["运行中   /p/随手问", "❯ NUC 的事给谁", "● 交给「查电压」了。"]);
        assert!(bar(&mut app).contains(&caps("⏎ 进入 • s 回话 • o 打开总台")), "{}", bar(&mut app));
        assert_eq!(app.thread_on.as_ref().map(|t| t.0.as_str()), Some("id-总台"));
        // The scratch group's own row shows the same thread: moving between the two isn't leaving it
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.group.as_deref(), Some("随手问"));
        let rows = reader_rows(&mut app, 150, 24);
        shows(&rows, &["❯ NUC 的事给谁", "● 交给「查电压」了。"]);
        assert!(app.thread_on.is_some() && app.reads.read_at("thread:id-总台").is_none());
        let b = bar(&mut app);
        assert!(b.contains(&caps("⏎ 进入 • s 回话 • o 打开总台 • Tab 换页 • e 写笔记")), "{b}");
        // Onto a project's row: now it is left, and marked seen
        press(&mut app, KeyCode::Char('j'));
        reader_rows(&mut app, 150, 24);
        assert_eq!(app.group.as_deref(), Some("fun"));
        assert!(app.thread_on.is_none() && app.reads.read_at("thread:id-总台").is_some());
        // A screen with no rail of its own: called out over the list, the row for all groups still leads with it
        let mut app = with_scratch(Some(State::Running));
        reader_rows(&mut app, 100, 24);
        press(&mut app, KeyCode::Char('h'));
        let rows = reader_rows(&mut app, 100, 24);
        assert!(rows.iter().any(|r| r.contains("@ 全部")), "{rows:#?}");
    }

    #[test]
    fn without_a_desk_the_row_for_all_groups_has_nothing_beside_it_and_only_offers_to_start_one() {
        // A scratch group with no router yet, and no scratch group at all: the same but for the bar
        for mut app in [with_scratch(None), fixture()] {
            let rows = reader_rows(&mut app, 150, 24);
            assert!(!rail_of(&rows).iter().any(|r| r.contains('@')), "{rows:#?}");
            press(&mut app, KeyCode::Char('h'));
            assert_eq!((app.group.as_deref(), app.on_rail()), (None, true));
            let rows = reader_rows(&mut app, 150, 24);
            assert!(!rows.iter().any(|r| r.split('│').any(|cell| cell.trim() == "❯")), "{rows:#?}");
            let b = bar(&mut app);
            assert!(b.contains(&caps("⏎ 进入 • o 新建总台")) && !b.contains(&caps("s 回")), "{b}");
            press(&mut app, KeyCode::Char('s'));
            assert!(app.prompt.is_none());
            assert_eq!(said(&app), "还没有总台 · o 新建");
        }
        // On the scratch group's own row the router it would get is called the desk too
        let mut app = with_scratch(None);
        keys(&mut app, "hj");
        assert_eq!(app.group.as_deref(), Some("随手问"));
        assert!(bar(&mut app).contains(&caps("⏎ 进入 • o 新建总台 • Tab 换页")), "{}", bar(&mut app));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(said(&app), "还没有总台 · o 新建");
        // A desk that isn't running, and one waiting for you
        let mut app = with_scratch(Some(State::Stopped));
        press(&mut app, KeyCode::Char('h'));
        let b = bar(&mut app);
        assert!(b.contains(&caps("⏎ 进入 • s 发话 • o 启动总台")), "{b}");
        press(&mut app, KeyCode::Char('s'));
        assert!(app.prompt.is_some());
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.start.take().map(|t| t.name).as_deref(), Some("总台"));
        let mut app = with_scratch(Some(State::Ask));
        press(&mut app, KeyCode::Char('h'));
        assert!(bar(&mut app).contains(&caps("⏎ 进入 • s 回答 • o 打开总台")), "{}", bar(&mut app));
        press(&mut app, KeyCode::Char('s'));
        assert!(matches!(&app.confirm, Some(Confirm::Answer { t, .. }) if t.name == "总台"));
        // ⏎ in the box: into the desk, not into the list of all groups
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.attach.take().as_deref(), app.on_rail()), (Some("总台"), true));
    }

    #[test]
    fn o_and_s_on_the_row_for_all_groups_open_the_desk_and_write_to_it() {
        let mut app = with_scratch(Some(State::Running));
        press(&mut app, KeyCode::Char('h'));
        reader_rows(&mut app, 150, 24);
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.attach.take().as_deref(), Some("总台"));
        // What it said so far is read in there: marked seen on the way in
        assert!(app.reads.read_at("thread:id-总台").is_some());
        // Back from it the cursor is still on that row
        app.came_back("总台", "$总台");
        assert_eq!((app.group.as_deref(), app.on_rail(), app.stay.is_none()), (None, true, true));
        // s: on the line under its thread; ⏎ sends, and you stay on the row
        press(&mut app, KeyCode::Char('s'));
        assert!(matches!(&app.prompt.as_ref().unwrap().ask, Ask::Reply { t, .. } if t.name == "总台"));
        typ(&mut app, "也问问电流");
        let rows = reader_rows(&mut app, 150, 24);
        assert!(rows.iter().any(|r| r.split('│').any(|cell| cell.trim() == "❯ 也问问电流▏")), "{rows:#?}");
        press(&mut app, KeyCode::Enter);
        let job = app.take_job().unwrap();
        assert!(matches!(&job, Job::Send { t, text, .. } if t.name == "总台" && text == "也问问电流"), "{job:?}");
        assert_eq!((app.group.as_deref(), app.on_rail()), (None, true));
        // A phone: the rail lies over that column, so the reply is written in a box over the rail
        let mut app = with_scratch(Some(State::Running));
        reader_rows(&mut app, 46, 30);
        keys(&mut app, "hs");
        typ(&mut app, "好");
        shows(&reader_rows(&mut app, 46, 30), &["@ 全部", "↩ 回话 · 总台", "好▏"]);
        // ? on that row lists the two keys, o by the desk's name; on a session's row (「问」, in fun) o is its group's router
        let mut app = with_scratch(Some(State::Running));
        keys(&mut app, "h?");
        let sheet = reader_rows(&mut app, 150, 60);
        shows(&sheet, &["打开 / 新建总台", "回话"]);
        keys(&mut app, "⎋l?");
        let sheet = reader_rows(&mut app, 150, 60);
        shows(&sheet, &["打开 / 新建 router"]);
        assert!(!sheet.iter().any(|r| r.contains("新建总台")), "{sheet:#?}");
        // The Trash is no place for either key
        let mut app = with_scratch(Some(State::Running));
        keys(&mut app, "hGo");
        assert!(app.attach.is_none() && app.new_draft.is_none());
        press(&mut app, KeyCode::Char('s'));
        assert!(app.prompt.is_none());
    }

    #[test]
    fn o_with_no_desk_opens_the_new_session_box_for_one_in_the_scratch_group() {
        let mut app = with_scratch(None);
        press(&mut app, KeyCode::Char('h'));
        press(&mut app, KeyCode::Char('o'));
        let rows = reader_rows(&mut app, 150, 40);
        // In the scratch group and its directory, with the tool N on that group's row would start: the one its latest session runs
        shows(&rows, &["新建总台", "Codex", "分组   随手问", "/h", "⏎ 新建"]);
        assert!(!rows.iter().any(|r| r.contains("^T") || r.contains("新分组") || r.contains("新建 router")), "{rows:#?}");
        let d = app.new_draft.as_ref().unwrap();
        assert!(d.router && d.desk && !app.pick);
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.dir.as_str(), r.group.as_str(), r.spec().as_str()), ("/h", "随手问", "codex"));
        assert!(r.router && !r.bind && !r.by_dir);
        // Made: back from it the cursor is on the row for all groups
        app.created(&r, &create::Created { id: "d1".into(), unsaid: None });
        app.came_back("d1", "");
        assert_eq!((app.group.as_deref(), app.on_rail()), (None, true));
        // From the scratch group's own row it is the same box
        let mut app = with_scratch(None);
        keys(&mut app, "hjo");
        assert!(app.new_draft.as_ref().is_some_and(|d| d.desk && d.group().0 == "随手问" && d.text == "/h"));
        // A scratch group nobody has started anything in: the tool of the row the list is on, as N there
        let mut v = sessions();
        v.iter_mut().for_each(|s| (s.tool, s.agent) = ("codex".into(), Agent::Codex));
        let mut app = App::new(String::new(), v).with_group_hints(asks_hints).with_scratch(|| "/h".into(), |_| Ok("随手问".into()));
        assert_eq!(app.scratch.as_deref(), Some("随手问"));
        keys(&mut app, "ho");
        let r = app.new_draft.take().unwrap().finish().unwrap();
        assert_eq!((r.dir.as_str(), r.group.as_str(), r.spec().as_str(), r.router), ("/h", "随手问", "codex", true));
    }

    /// `with_scratch`, with more in the scratch group: a session that started one of its own, one whose parent is gone, and a
    /// project under the group
    fn loose(desk: Option<State>) -> App {
        let mut v = std::mem::take(&mut with_scratch(desk).all);
        let n = v.len() as i64;
        let mut add = |i: i64, group: &str, rank: usize, mut s: Session| {
            (s.group, s.slot) = (group.into(), (rank, n + i, 0));
            v.push(s);
        };
        add(0, "随手问", 2, mk("订机票", "随手问", State::Idle, 40));
        add(1, "随手问", 2, Session { up: Up::Under("订机票".into(), "订机票".into()), ..mk("比价", "随手问", State::Idle, 40) });
        add(2, "随手问", 2, Session { up: Up::Gone, ..mk("孤儿", "随手问", State::Idle, 40) });
        add(3, "随手问/rss", 3, mk("订阅", "随手问/rss", State::Idle, 40));
        let hints = || {
            let mut h = asks_hints();
            h.groups.push(("随手问/rss".into(), "/h/rss".into()));
            h
        };
        App::new(String::new(), v).with_group_hints(hints).with_scratch(|| "/h".into(), |_| Ok("随手问".into()))
    }

    /// The list's session rows as the interface and `aca ui -l` both build them, each from its tree branch (or its state's
    /// mark) to the end of its title
    fn drawn(app: &mut App) -> Vec<String> {
        reader_rows(app, 150, 30);
        let view: Vec<&Session> = app.view.iter().map(|&i| &app.all[i]).collect();
        let rows = list_rows(&view, app.sectioned(), 80, None, 0, true);
        let text = |r: &list::Row| r.lines[0].iter().map(|(t, _)| t.as_str()).collect::<String>();
        // After the row's number, up to the gap that follows the title
        let cut = |line: String| line.trim_start().split_once(' ').map_or("", |x| x.1).split("  ").next().unwrap_or("").trim().to_string();
        rows.iter().filter(|r| r.pos.is_some()).map(|r| cut(text(r))).collect()
    }

    #[test]
    fn the_scratch_groups_mark_leads_its_name_and_stands_for_it_in_the_names_of_the_groups_under_it() {
        let mut app = loose(None);
        let rows = reader_rows(&mut app, 150, 30);
        // The rail: the mark before the scratch group's name; a group under it is a row further in, by its last level
        let rail = rail_of(&rows);
        assert!(rail.iter().any(|r| r.contains("  ⌂ 随手问")) && rail.iter().any(|r| r.contains("      rss")), "{rail:#?}");
        // The list's headers, on the desktop and as `aca ui -l` prints them: the group's whole path is never written
        let view: Vec<&Session> = app.view.iter().map(|&i| &app.all[i]).collect();
        let heads = |desk: bool| -> Vec<String> {
            let rows = list_rows(&view, true, 60, None, 0, desk);
            rows.iter().filter(|r| r.head.is_some()).map(|r| r.lines[0].iter().map(|(t, _)| t.as_str()).collect()).collect()
        };
        let (wide, plain) = (heads(true), heads(false));
        assert!(wide[0].starts_with(" ⌂ 随手问 ") && wide[1].starts_with("   ⌂/rss "), "{wide:#?}");
        assert_eq!(plain[..2], [" ⌂ 随手问  4", " ▾ ⌂/rss  1"], "{plain:#?}");
        let list: Vec<String> = rows.iter().map(|r| r.chars().skip(29).take(58).collect()).collect();
        assert!(list.iter().any(|r| r.contains(" ⌂ 随手问")) && list.iter().any(|r| r.contains("   ⌂/rss")), "{list:#?}");
        assert!(!list.iter().any(|r| r.contains("随手问/rss")), "{list:#?}");
        // Beside the rail's rows: the scratch group's head leads with its mark, the one under it is named by it
        keys(&mut app, "hj");
        assert_eq!(app.group.as_deref(), Some("随手问"));
        assert_eq!(beside(&mut app, 150, 30)[0], "⌂ 随手问");
        keys(&mut app, "j");
        assert_eq!(app.group.as_deref(), Some("随手问/rss"));
        assert_eq!(beside(&mut app, 150, 30)[0], "▸ ⌂/rss");
        // A screen with no rail says where the list went in the bar, the same way
        let mut app = loose(None);
        reader_rows(&mut app, 100, 30);
        app.set_group(Some("随手问/rss".into()));
        assert!(bar(&mut app).contains("分组:⌂/rss"), "{}", bar(&mut app));
        // With no scratch group every name is the path, as before
        let mut app = App::new(String::new(), std::mem::take(&mut loose(None).all));
        let rows = reader_rows(&mut app, 150, 30);
        assert!(!rows.iter().any(|r| r.contains('⌂')) && rows.iter().any(|r| r.contains("随手问/rss")), "{rows:#?}");
    }

    #[test]
    fn the_scratch_groups_sessions_that_hang_under_nothing_are_drawn_under_the_desk_which_leads_them() {
        let mut app = loose(Some(State::Running));
        // The desk first, wherever the registry has it; the rest in their own order one level in, what one of them started a
        // level further; the project under the group is its own section, as it was
        assert_eq!(order(&app)[..6], ["总台", "查电压", "订机票", "比价", "孤儿", "订阅"]);
        let rows = drawn(&mut app);
        assert_eq!(rows[..6], ["@ 总台", "├ ○ 查电压", "├ ○ 订机票", "│ └ ○ 比价", "└ ○ 孤儿", "○ 订阅"], "{rows:#?}");
        // Drawn only: none of them is the desk's child (its parent is as the registry has it)
        assert!(app.all.iter().filter(|s| s.group == "随手问" && s.title != "比价").all(|s| s.under().is_none()));
        // The group's own page draws them the same
        keys(&mut app, "hjl");
        assert_eq!(app.group.as_deref(), Some("随手问"));
        assert_eq!(drawn(&mut app), ["@ 总台", "├ ○ 查电压", "├ ○ 订机票", "│ └ ○ 比价", "└ ○ 孤儿"]);
        // On screen too
        let rows = reader_rows(&mut app, 150, 30);
        shows(&rows, &["@ 总台", "├ ○ 查电压", "│ └ ○ 比价", "└ ○ 孤儿"]);
        // No desk: flat, as before
        let mut app = loose(None);
        assert_eq!(order(&app)[..5], ["查电压", "订机票", "比价", "孤儿", "订阅"]);
        assert_eq!(drawn(&mut app)[..5], ["○ 查电压", "○ 订机票", "└ ○ 比价", "○ 孤儿", "○ 订阅"]);
        // A desk that isn't listed (stopped, and only the open ones show): flat too
        let mut app = loose(Some(State::Stopped));
        assert_eq!(drawn(&mut app)[..3], ["○ 查电压", "○ 订机票", "└ ○ 比价"]);
        // Pinned, the desk is in the pinned block: nothing is drawn under it there or in its group
        let mut app = loose(Some(State::Running));
        pin(&mut app, "总台");
        let rows = drawn(&mut app);
        assert_eq!(rows[..5], ["○ 查电压", "○ 订机票", "└ ○ 比价", "○ 孤儿", "○ 订阅"], "{rows:#?}");
        assert!(rows.contains(&"@ 总台".to_string()), "{rows:#?}");
    }

    #[test]
    fn rows_drawn_under_the_desk_still_move_and_go_as_top_level_sessions_and_the_desk_stays_first() {
        let mut app = loose(Some(State::Running));
        // ⇧↓ on one of them swaps it with the next that hangs under nothing, as before
        select(&mut app, "查电压");
        shift(&mut app, KeyCode::Down);
        assert!(matches!(app.take_job(), Some(Job::Order { t, by, after: true }) if t.title == "查电压" && by == "id-订机票"));
        // The first of them has nothing to swap with above: the desk isn't one of them
        let mut app = loose(Some(State::Running));
        select(&mut app, "查电压");
        shift(&mut app, KeyCode::Up);
        assert!(app.take_job().is_none() && app.reorders.is_empty());
        // The desk itself doesn't move
        select(&mut app, "总台");
        shift(&mut app, KeyCode::Down);
        assert!(app.take_job().is_none() && app.reorders.is_empty());
        assert_eq!(said(&app), "总台固定在最前");
        // d on one of them asks about that one alone: nothing follows it the way a child would
        select(&mut app, "订机票");
        assert_eq!(app.current().map(|s| (s.title.as_str(), s.under())), Some(("订机票", None)));
        // Its preview doesn't call it a child session
        let rows = reader_rows(&mut app, 150, 30);
        assert!(!rows.iter().any(|r| r.contains("子会话")), "{rows:#?}");
    }

    thread_local! {
        /// What was written to config.toml (the fake config_set records it here)
        static WROTE: std::cell::RefCell<Vec<(String, String, String)>> = const { std::cell::RefCell::new(vec![]) };
    }

    #[test]
    fn o_with_no_scratch_directory_asks_for_it_first_and_goes_on_to_the_desks_box() {
        let ensure = |dir: &str| if dir.ends_with("/nope") { Err("无法创建目录".to_string()) } else { Ok("随手问".to_string()) };
        let set = |sec: &str, key: &str, v: toml::Value| {
            WROTE.with(|w| w.borrow_mut().push((sec.into(), key.into(), v.to_string())));
            Ok(())
        };
        let mut app = fixture().with_group_hints(asks_hints).with_scratch(String::new, ensure).with_settings(|| true, Vec::new, set, Vec::new);
        assert!(app.scratch.is_none());
        keys(&mut app, "ho");
        assert!(matches!(app.prompt.as_ref().map(|p| (&p.ask, p.text.as_str())), Some((Ask::ScratchDir { desk: true }, ""))));
        shows(&reader_rows(&mut app, 150, 40), &["随手问目录"]);
        // A directory that can't get its group: nothing written, the box stays
        typ(&mut app, "~/nope");
        press(&mut app, KeyCode::Enter);
        assert!(app.prompt.as_ref().is_some_and(|p| p.hint == "无法创建目录") && app.new_draft.is_none());
        assert!(WROTE.with(|w| w.borrow().is_empty()));
        // Given: written down, and the desk's box opens in it
        ctrl(&mut app, 'u');
        typ(&mut app, "/h");
        press(&mut app, KeyCode::Enter);
        assert_eq!(WROTE.with(|w| w.borrow_mut().drain(..).collect::<Vec<_>>()), [("scratch".to_string(), "dir".to_string(), "\"/h\"".to_string())]);
        assert!(app.prompt.is_none() && app.refresh);
        let d = app.new_draft.as_ref().unwrap();
        assert_eq!((d.desk, d.text.as_str(), d.group().0.as_str()), (true, "/h", "随手问"));
        shows(&reader_rows(&mut app, 150, 40), &["新建总台", "分组   随手问"]);
        // Esc in the directory's box: nothing asked for, nothing written
        let mut app = fixture().with_scratch(String::new, ensure).with_settings(|| true, Vec::new, set, Vec::new);
        keys(&mut app, "ho⎋");
        assert!(app.prompt.is_none() && app.new_draft.is_none() && WROTE.with(|w| w.borrow().is_empty()));
        // The group that works in the directory given has a router already: that one is the desk, no box
        let mut v = sessions();
        v.iter_mut().find(|s| s.title == "跑").unwrap().router = true;
        let mut app = App::new(String::new(), v).with_scratch(String::new, |_| Ok("work".into())).with_settings(|| true, Vec::new, set, Vec::new);
        keys(&mut app, "ho");
        typ(&mut app, "/p/work");
        press(&mut app, KeyCode::Enter);
        assert!(app.prompt.is_none() && app.new_draft.is_none() && app.refresh);
        WROTE.with(|w| w.borrow_mut().clear());
    }

    #[test]
    fn a_stopped_claude_router_accepts_a_reply_or_starts_with_o() {
        let mut v = sessions();
        let r = v.iter_mut().find(|s| s.title == "跑").unwrap();
        (r.router, r.state) = (true, State::Stopped);
        let mut app = App::new(String::new(), v);
        keys(&mut app, "hjj");
        let b = bar(&mut app);
        assert!(b.contains(&caps("⏎ 进入 • s 发话 • o 启动 router • Tab 换页")), "{b}");
        press(&mut app, KeyCode::Char('s'));
        assert!(app.prompt.is_some());
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.start.take().map(|t| t.name).as_deref(), Some("跑"));
        // Stopping while the reply box is open still sends the words through the inbox.
        let mut app = with_router();
        keys(&mut app, "hjjs");
        typ(&mut app, "先停一下");
        let mut v = app.all.clone();
        v.iter_mut().find(|s| s.title == "跑").unwrap().state = State::Stopped;
        app.set_sessions(v);
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.take_job(), Some(Job::Send { text, .. }) if text == "先停一下"));
    }

    #[test]
    fn o_on_a_group_without_a_router_opens_the_new_session_box_for_one() {
        let mut app = fixture();
        keys(&mut app, "hj");
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("fun"), true));
        let b = bar(&mut app);
        assert!(b.contains(&caps("⏎ 进入 • o 新建 router • Tab 换页 • e 写笔记 • N 在组里新建 • r 改名")), "{b}");
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(said(&app), "还没有 router · o 新建");
        press(&mut app, KeyCode::Char('o'));
        let rows = reader_rows(&mut app, 150, 40);
        shows(&rows, &["新建 router", "分组   fun", "/p/fun", "⏎ 新建"]);
        // An agent, never a shell; and its group is this one whatever the directory
        assert!(!rows.iter().any(|r| r.contains("^T") || r.contains("新分组")), "{rows:#?}");
        ctrl(&mut app, 't');
        typ(&mut app, "/sub");
        let d = app.new_draft.as_ref().unwrap();
        assert_eq!((d.is_shell(), d.group().0.as_str()), (false, "fun"));
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.dir.as_str(), r.group.as_str(), r.spec().as_str()), ("/p/fun/sub", "fun", "claude"));
        assert!(r.router && !r.bind && !r.by_dir);
        // Made: back from it the cursor is on the group's row; a brief that didn't reach it is its draft
        let made = create::Created { id: "r1".into(), unsaid: Some(("你是 router".into(), "会话一直未启动完成,未发送".into())) };
        app.created(&r, &made);
        assert_eq!(app.notes.get("r1"), Some("你是 router"));
        app.came_back("r1", "");
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("fun"), true));
        // An ordinary new session isn't stayed away from
        let plain = NewReq { router: false, ..r };
        app.created(&plain, &create::Created { id: "n1".into(), unsaid: None });
        app.came_back("n1", "");
        assert!(!app.on_rail() && app.selected.as_deref() == Some("n1"));
        // The sessions in no group, and the plain tmux sessions' section, can have none
        assert!(can_route("fun/aca") && !can_route("") && !can_route("-"));
        // An empty group with no directory yet: straight to picking one, as N there; what's picked starts the router in it
        let mut v = sessions();
        v.push(crate::data::blank_row("lab", 2));
        let mut app = App::new(String::new(), v);
        reader_rows(&mut app, 150, 40);
        keys(&mut app, " Lo");
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("lab"), true));
        assert!(std::mem::take(&mut app.pick) && app.new_draft.as_ref().is_some_and(|d| d.router));
        app.picked(Ok(Some(Picked::Open("/p/lab".into()))));
        let r = app.create.take().unwrap();
        assert_eq!((r.dir.as_str(), r.group.as_str(), r.router), ("/p/lab", "lab", true));
    }

    #[test]
    fn s_on_a_groups_row_writes_to_its_router_on_the_line_under_its_thread() {
        let mut app = with_router();
        keys(&mut app, "hjjs");
        assert!(matches!(&app.prompt.as_ref().unwrap().ask, Ask::Reply { t, .. } if t.name == "跑"));
        typ(&mut app, "登录先放放");
        let rows = reader_rows(&mut app, 150, 24);
        // In the column, under the exchange; the bar has the reply's keys
        assert!(rows.iter().any(|r| r.split('│').any(|cell| cell.trim() == "❯ 登录先放放▏")), "{rows:#?}");
        shows(&rows, &["● 交给「闲」了。", &caps("⏎ 发送 • ^J 换行 • Esc 存成便签")]);
        press(&mut app, KeyCode::Enter);
        let job = app.take_job().unwrap();
        assert!(matches!(&job, Job::Send { t, text, key, .. } if t.name == "跑" && text == "登录先放放" && key == "id-跑"), "{job:?}");
        assert!(app.on_rail());
        // Esc: not sent, it stays as the router's draft and shows on that line
        app.busy = None;
        press(&mut app, KeyCode::Char('s'));
        typ(&mut app, "再想想");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.notes.get("id-跑"), Some("登录先放放再想想"));
        let rows = reader_rows(&mut app, 150, 24);
        assert!(rows.iter().any(|r| r.split('│').any(|cell| cell.trim() == "❯ 登录先放放再想想")), "{rows:#?}");
        // No column beside the list: the same box as on a session's row, named after the router
        let mut app = with_router();
        (app.preview, app.preview_narrow) = (Preview::Off, Preview::Off);
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('s'));
        shows(&reader_rows(&mut app, 100, 24), &["↩ 回话 · 跑"]);
        // A phone: the rail called out lies over the whole screen, that column included, so the box is laid over the rail
        let mut app = with_router();
        reader_rows(&mut app, 46, 30);
        keys(&mut app, "hjjs");
        typ(&mut app, "好");
        let rows = reader_rows(&mut app, 46, 30);
        shows(&rows, &["@ work", "↩ 回话 · 跑", "好▏", "⏎ 发送 • ^J 换行"]);
        // So is a note being written for the group there
        press(&mut app, KeyCode::Esc);
        keys(&mut app, "e记");
        shows(&reader_rows(&mut app, 46, 30), &["@ work", "笔记 · work", "记▏"]);
    }

    #[test]
    fn a_router_waiting_for_you_is_answered_from_its_groups_row_and_enter_there_goes_into_it() {
        let mut v = sessions();
        let r = v.iter_mut().find(|s| s.title == "跑").unwrap();
        (r.router, r.state, r.question) = (true, State::Ask, "先做哪个？".into());
        let mut app = App::new(String::new(), v);
        keys(&mut app, "hjj");
        assert!(has_caps(&bar(&mut app), "⏎ 进入 • s 回答 • o 打开 router"), "{}", bar(&mut app));
        press(&mut app, KeyCode::Char('s'));
        assert!(matches!(&app.confirm, Some(Confirm::Answer { t, question, .. }) if t.name == "跑" && question == "先做哪个？"));
        // ⏎ in the box: into the router, not into the group
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.attach.take().as_deref(), app.on_rail()), (Some("跑"), true));
    }

    #[test]
    fn what_came_since_you_last_looked_is_bold_and_your_own_words_never_are() {
        let r = router();
        let p = RouterPane { router: &r, dir: None, branch: None, pins: &[], tally: None, seen: 941, now: 1060, writing: None, kept: None };
        let lines = router_pane(&p, &names, 60, 16);
        let bold = |l: &Line| l.spans.iter().any(|s| s.style.add_modifier.contains(Modifier::BOLD));
        let fresh: Vec<String> = lines.iter().filter(|l| bold(l)).map(|l| text(std::slice::from_ref(l))[0].clone()).collect();
        assert_eq!(fresh.len(), 3, "{fresh:?}");
        assert!(fresh[0].starts_with("● 拉过来") && fresh[1].starts_with("@ → fix-1117-panel") && fresh[2].starts_with("fix-1117-panel ❯"));
        // Never looked at before: nothing is news yet
        let p = RouterPane { seen: u64::MAX, ..p };
        assert!(!router_pane(&p, &names, 60, 16).iter().any(bold));
    }

    #[test]
    fn what_came_in_through_a_channel_names_it_before_how_long_ago_and_the_words_give_way() {
        let mut r = router();
        r.thread = vec![
            Entry { via: "Discord".into(), ..entry(940, Kind::You, "", "帮我看下周去成都的机票，周二或周三走") },
            Entry { via: "Discord".into(), pending: true, id: "m9".into(), ..entry(1000, Kind::You, "", "墨水屏的事") },
            entry(1050, Kind::You, "", "这句是在这里说的"),
        ];
        let p = RouterPane {
            router: &r,
            dir: None,
            branch: None,
            pins: &[],
            tally: None,
            seen: u64::MAX,
            now: 1060,
            writing: None,
            kept: None,
        };
        let got = text(&router_pane(&p, &names, 60, 10));
        let said: Vec<String> = squeezed(&got).into_iter().filter(|l| l.starts_with("❯ ")).collect();
        assert_eq!(said, ["❯ 帮我看下周去成都的机票，周二或周三走 Discord 2分", "❯ 墨水屏的事 未送达 Discord 1分", "❯ 这句是在这里说的 10秒"]);
        assert!(got.iter().filter(|l| l.starts_with("❯ ")).all(|l| l.width() == 60), "{got:?}");
        // Narrow: the name and the time stay, the words are cut
        let got = text(&router_pane(&p, &names, 30, 10));
        assert!(got.iter().any(|l| l.starts_with("❯ 帮我看下周") && l.contains('…') && l.ends_with("Discord  2分") && l.width() == 30), "{got:?}");
    }

    #[test]
    fn a_thread_longer_than_the_column_shows_its_latest_and_a_long_reply_takes_two_lines() {
        let mut r = router();
        r.thread = (0..30).map(|i| entry(100 + i, Kind::To, "a1", &format!("第 {i} 件"))).collect();
        r.thread.push(entry(131, Kind::Reply, "", &"都交出去了，".repeat(12)));
        let p = RouterPane {
            router: &r,
            dir: None,
            branch: None,
            pins: &[],
            tally: None,
            seen: u64::MAX,
            now: 200,
            writing: Some("好，周五前"),
            kept: Some("写了一半的"),
        };
        let got = text(&router_pane(&p, &names, 40, 10));
        assert_eq!(got.len(), 10);
        assert_eq!(got[0], "运行中 · 1 分钟");
        // The last entries, the reply cut after its second line, then the reply being written
        assert!(got[5].starts_with("@ → NUC 待机功耗  第 29 件"), "{got:?}");
        assert!(got[6].starts_with("● 都交出去了") && got[7].starts_with("  ") && got[7].ends_with('…'), "{got:?}");
        assert_eq!(got[9], "❯ 好，周五前▏");
        assert!(got.iter().all(|l| l.width() <= 40), "{got:?}");
        // Several lines being written: the last few, where the cursor is; not being written, what was left unsent shows
        let p = RouterPane { writing: Some("一\n二\n三\n四\n五"), ..p };
        let got = text(&router_pane(&p, &names, 40, 10));
        assert_eq!(got[6..], ["❯ 二", "  三", "  四", "  五▏"], "{got:?}");
        let p = RouterPane { writing: None, kept: Some("写了一半的\n第二行"), ..p };
        assert_eq!(text(&router_pane(&p, &names, 40, 10))[9], "❯ 写了一半的");
        // What the router did elsewhere, what it started, what a session it sent out reports, a failure, a question
        r.thread = vec![
            Entry { aux: "other".into(), ..entry(100, Kind::Pulled, "a1", "nuc") },
            entry(101, Kind::Started, "b2", "面板"),
            entry(102, Kind::Notice, "b2", "turn.done"),
            entry(103, Kind::Failed, "", "rate limit"),
            entry(104, Kind::Asks, "", "先做哪个？"),
        ];
        let p =
            RouterPane { router: &r, dir: None, branch: None, pins: &[], tally: None, seen: u64::MAX, now: 200, writing: None, kept: None };
        let got = text(&router_pane(&p, &names, 50, 12));
        let lead: Vec<&str> = got[2..7].iter().map(|l| l.rsplit_once("  ").map_or(l.as_str(), |x| x.0).trim_end()).collect();
        assert_eq!(lead, ["@ NUC 待机功耗 → other", "@ → fix-1117-panel  新会话", "fix-1117-panel · 跑完一轮", "✕ rate limit", "◆ 先做哪个？"]);
    }
}
