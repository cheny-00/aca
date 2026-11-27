//! The conversation record page (`Z`): turns and the steps inside them, laid out in blocks and drawn, following new turns as
//! they come in.

use super::*;

/// `Z` session record (see `trace`). Desktop, two panes: the conversation on the left (one block per turn: what you said, what Claude
/// said last, a line of numbers),
/// the selected turn's steps on the right (one block per call, per tool); `→` goes into the right side step by step, `←` comes back.
/// Phones don't fit two panes: the whole screen is the conversation, `⏎` shows this turn's steps full screen. The record is read in the
/// background by the run loop; each later refresh reads only the newly added part.
/// `⏎` on a step that started a subagent goes in to view its own record in the same two panes; `Esc` comes back to carry on
pub(super) struct TraceView {
    /// tmux session name
    pub(super) name: String,
    t: Option<crate::trace::Trace>,
    /// Why it couldn't be read
    err: Option<String>,
    /// Needs a background read: just opened / session changed
    want: bool,
    loading: bool,
    /// Unfolded on the left: turns shown in full, the block being copied
    open: HashSet<usize>,
    /// Unfolded on the right: calls (full thinking and text), tools (diffs with context, file content, first 200 lines of output)
    unfold: HashSet<usize>,
    /// How many times unfold changed (the steps laid out on the right are relaid with it)
    unfold_gen: u64,
    /// The conversation on the left
    pub(super) chat: Blocks,
    /// The steps on the right
    pub(super) steps: Blocks,
    /// Keys go to the right side (on a phone = the whole screen is steps)
    in_steps: bool,
    /// Whose steps the right side shows (start from the top when it changes)
    steps_of: Option<usize>,
    pub(super) detail: Option<TraceDetail>,
    /// Whether the last draw was two panes or phone
    pub(super) wide: bool,
    /// Which column the left pane ends at (which side the mouse scrolls)
    pub(super) split: u16,
    /// Laid-out conversation: turn → laid-out rows
    chat_cache: HashMap<usize, ChatLaid>,
    /// Laid-out steps (of the item on the right)
    steps_cache: Option<StepsLaid>,
    /// Which palette was used for layout (address of `theme::pal()`): after a theme change, the colors in laid-out rows are stale
    pal: usize,
    /// Which subagent we're in: the tools along the way that started it (empty = the session's own record)
    path: Vec<usize>,
    /// How far each level had been viewed before entering a subagent (to carry on when back)
    saved: Vec<Level>,
    /// Opened from a subagent row in the list: once read, go straight into this subagent (its id)
    goto: Option<String>,
    /// Which level we landed on when entering that way (0 = didn't enter that way): Esc at this level goes straight back to the list,
    /// without backing out level by level
    home: usize,
}

/// What the level looked like before entering a subagent
struct Level {
    open: HashSet<usize>,
    unfold: HashSet<usize>,
    unfold_gen: u64,
    chat: Blocks,
    steps: Blocks,
    in_steps: bool,
    steps_of: Option<usize>,
    chat_cache: HashMap<usize, ChatLaid>,
    steps_cache: Option<StepsLaid>,
}

/// One laid-out turn of the conversation. Laid out uncolored first (to measure height), colored once it's visible (see `hl::plain`)
struct ChatLaid {
    w: usize,
    ver: u64,
    /// Full text (o)
    full: bool,
    lines: Vec<Styled>,
    /// Whether it was cut while folded (if not, o makes no difference)
    clipped: bool,
    /// How many rows are colored (from the top; coloring is fed from the start of a code block, so only the later part can be skipped)
    colored: usize,
}

/// Laid-out steps on the right: which item, width, version, how many times unfold changed, block by block
struct StepsLaid {
    item: usize,
    w: usize,
    ver: u64,
    folds: u64,
    blocks: Vec<StepLaid>,
}

struct StepLaid {
    node: usize,
    lines: Vec<Styled>,
    /// Whether o works
    fold: bool,
    /// How many rows are colored (same as ChatLaid)
    colored: usize,
    /// What the layout depends on (trace::step_sig): when relaying a whole turn, identical ones are reused
    sig: u64,
}

/// One item full screen (⏎)
pub(super) struct TraceDetail {
    node: usize,
    lines: Vec<crate::trace::Para>,
    wrapped: Vec<Styled>,
    wrap_w: usize,
    /// Rows from the top
    pub(super) scroll: usize,
    view_h: usize,
}

impl TraceDetail {
    fn new(node: usize) -> Self {
        TraceDetail { node, lines: vec![], wrapped: vec![], wrap_w: 0, scroll: 0, view_h: 0 }
    }
}

/// A column of selectable blocks (the conversation on the left, the steps on the right). Keys go by the last draw: each block is
/// (node, first row, row it ends before)
#[derive(Default)]
pub(super) struct Blocks {
    items: Vec<(usize, usize, usize)>,
    /// Selected node
    sel: usize,
    /// Row at the very top
    top: usize,
    view_h: usize,
    /// Sticking to the latest: when new ones arrive, select the last block, stuck to the bottom
    follow: bool,
    /// The selected block was just unfolded: after the next layout, show the whole block if possible (head not off screen)
    reveal: bool,
}

impl Blocks {
    fn at(&self) -> Option<usize> {
        self.items.iter().position(|x| x.0 == self.sel)
    }

    fn total(&self) -> usize {
        self.items.last().map_or(0, |x| x.2)
    }

    /// ↑↓: a block taller than the screen scrolls inside first, and moves to the next block only at its end; after the last block,
    /// go on following the latest
    fn go(&mut self, down: bool) {
        let Some(k) = self.at() else { return };
        let h = self.view_h.max(1);
        let (_, s, e) = self.items[k];
        self.follow = false;
        if down {
            if e > self.top + h {
                self.top += 1;
            } else if let Some(&(n, s2, e2)) = self.items.get(k + 1) {
                self.sel = n;
                if e2 > self.top + h {
                    self.top = if e2 - s2 > h { s2 } else { e2 - h };
                }
            } else {
                self.follow = true;
            }
        } else if s < self.top {
            self.top -= 1;
        } else if k > 0 {
            let (n, s2, e2) = self.items[k - 1];
            self.sel = n;
            if s2 < self.top {
                self.top = if e2 - s2 > h { e2 - h } else { s2 };
            }
        }
    }

    /// Scroll the whole column d rows; if the selected one scrolls out, select the first block on screen
    pub(super) fn scroll(&mut self, d: isize) {
        let h = self.view_h.max(1);
        self.top = self.top.saturating_add_signed(d).min(self.total().saturating_sub(h));
        self.follow = false;
        let top = self.top;
        let gone = self.at().is_none_or(|k| self.items[k].2 <= top || self.items[k].1 >= top + h);
        if gone && let Some(&(n, ..)) = self.items.iter().find(|x| x.2 > top) {
            self.sel = n;
        }
    }

    fn home(&mut self) {
        if let Some(&(n, ..)) = self.items.first() {
            self.sel = n;
            self.top = 0;
            self.follow = false;
        }
    }

    /// Select a block and put its head at the very top
    fn jump(&mut self, n: usize) {
        self.sel = n;
        if let Some(k) = self.at() {
            self.top = self.items[k].1;
        }
        self.follow = false;
    }

    /// Laid out: record where each block is; when following the latest, select the last block, stuck to the bottom; if the selected one
    /// is gone (folded away), select the level above it; if the whole block is off screen, bring its head into view
    fn lay(&mut self, t: &crate::trace::Trace, sizes: &[(usize, usize)], h: usize) {
        let mut y = 0;
        self.items = sizes
            .iter()
            .map(|&(n, len)| {
                y += len;
                (n, y - len, y)
            })
            .collect();
        self.view_h = h;
        if self.follow {
            if let Some(&(n, ..)) = self.items.last() {
                self.sel = n;
            }
            self.top = y.saturating_sub(h);
        }
        let mut x = self.sel;
        while self.items.iter().all(|i| i.0 != x) {
            match t.nodes.get(x).and_then(|n| n.parent) {
                Some(p) => x = p,
                None => {
                    x = self.items.first().map_or(0, |i| i.0);
                    break;
                }
            }
        }
        self.sel = x;
        if let Some(k) = self.at() {
            let (_, s, e) = self.items[k];
            if e <= self.top || s >= self.top + h {
                self.top = s;
            }
            if std::mem::take(&mut self.reveal) && e > self.top + h {
                self.top = s.min(e - h);
            }
        }
        self.top = self.top.min(y.saturating_sub(h));
    }
}

impl TraceView {
    pub(super) fn new(name: String) -> Self {
        TraceView {
            name,
            t: None,
            err: None,
            want: true,
            loading: true,
            open: HashSet::new(),
            unfold: HashSet::new(),
            unfold_gen: 0,
            chat: Blocks { follow: true, ..Blocks::default() },
            steps: Blocks::default(),
            in_steps: false,
            steps_of: None,
            detail: None,
            wide: true,
            split: 0,
            chat_cache: HashMap::new(),
            steps_cache: None,
            pal: 0,
            path: vec![],
            saved: vec![],
            goto: None,
            home: 0,
        }
    }

    /// The record of the level being viewed (a subagent's, once inside one)
    fn tr(&self) -> Option<&crate::trace::Trace> {
        self.t.as_ref().map(|t| t.at(&self.path))
    }

    /// Whether this level is running (subagent: the session is running and it hasn't finished)
    fn live(&self, live: bool, now: i64) -> bool {
        self.t.as_ref().is_some_and(|t| t.live_at(&self.path, live, now))
    }

    /// ⏎ on a step that started a subagent: go in and view its record (same two panes, at the latest), noting how far this level was
    /// viewed
    fn enter_sub(&mut self, tool: usize) -> bool {
        if self.tr().and_then(|t| t.sub_of(tool)).is_none() {
            return false;
        }
        use std::mem::take;
        let lv = Level {
            open: take(&mut self.open),
            unfold: take(&mut self.unfold),
            unfold_gen: self.unfold_gen,
            chat: take(&mut self.chat),
            steps: take(&mut self.steps),
            in_steps: self.in_steps,
            steps_of: self.steps_of.take(),
            chat_cache: take(&mut self.chat_cache),
            steps_cache: self.steps_cache.take(),
        };
        self.saved.push(lv);
        self.path.push(tool);
        self.chat = Blocks { follow: true, ..Blocks::default() };
        self.in_steps = true;
        self.detail = None;
        true
    }

    /// Back to the level above, carrying on where we were before going in
    fn leave_sub(&mut self) -> bool {
        let Some(lv) = self.saved.pop() else { return false };
        self.path.pop();
        self.open = lv.open;
        self.unfold = lv.unfold;
        self.unfold_gen = lv.unfold_gen;
        self.chat = lv.chat;
        self.steps = lv.steps;
        self.in_steps = lv.in_steps;
        self.steps_of = lv.steps_of;
        self.chat_cache = lv.chat_cache;
        self.steps_cache = lv.steps_cache;
        self.detail = None;
        true
    }

    /// Read: stick to the latest
    fn set(&mut self, t: crate::trace::Trace) {
        let name = std::mem::take(&mut self.name);
        let goto = self.goto.take();
        *self = TraceView::new(name);
        self.loading = false;
        self.want = false;
        self.t = Some(t);
        if let Some(id) = goto {
            self.enter_to(&id);
        }
    }

    /// Go into subagent id: level by level through the tools along the way that started it (nested ones go through the levels above),
    /// each level stopping at the turn that started it, so Esc comes back there
    fn enter_to(&mut self, id: &str) {
        fn path(t: &crate::trace::Trace, id: &str) -> Option<Vec<usize>> {
            (0..t.nodes.len()).find_map(|i| {
                let s = t.sub_of(i)?;
                if s.id == id {
                    return Some(vec![i]);
                }
                let mut p = path(&s.t, id)?;
                p.insert(0, i);
                Some(p)
            })
        }
        let Some(p) = self.t.as_ref().and_then(|t| path(t, id)) else { return };
        for tool in p {
            if let Some(turn) = self.tr().map(|t| crate::trace::turn_of(t, tool)) {
                self.chat.sel = turn;
                self.chat.follow = false;
            }
            if !self.enter_sub(tool) {
                break;
            }
        }
        // Stop on the left (what was sent to it, what it handed back); the right side shows its steps as usual; one Esc goes back to the
        // list
        self.in_steps = false;
        self.home = self.path.len();
    }

    /// Read the newly added part (when following the latest, the draw selects the end by itself)
    fn tail(&mut self) {
        let Some(t) = self.t.as_mut() else { return };
        if !matches!(t.tail(), Ok(true)) {
            return;
        }
        if let Some(d) = self.detail.as_mut() {
            // Full screen is open: the content may have grown (a tool got its result); reread on the next draw
            d.wrap_w = 0;
            d.lines.clear();
        }
    }

    /// The column keys go to
    pub(super) fn cur(&mut self) -> &mut Blocks {
        if self.in_steps { &mut self.steps } else { &mut self.chat }
    }

    /// Move the left side to the next / previous turn (not stopping at the compaction row)
    fn turn_step(&mut self, next: bool) -> bool {
        let Some(t) = self.t.as_ref().map(|t| t.at(&self.path)) else { return false };
        let Some(k) = self.chat.at() else { return false };
        let turnish = |x: &&(usize, usize, usize)| matches!(t.nodes[x.0].kind, crate::trace::Kind::Turn | crate::trace::Kind::Copied);
        let items = &self.chat.items;
        let to = if next { items.iter().skip(k + 1).find(turnish) } else { items[..k].iter().rev().find(turnish) };
        let Some(&(n, ..)) = to else { return false };
        self.chat.jump(n);
        // The right side switches to this turn's steps, from the top
        self.steps = Blocks { sel: n, ..Blocks::default() };
        self.steps_of = Some(n);
        true
    }

    /// Whether the content can change: in the last turn (still growing) it changes with how far the record is read and whether the
    /// session is running; earlier turns usually don't change,
    /// unless a background subagent they started is still growing, or they were rolled back later
    fn version(&self, i: usize, live: bool) -> u64 {
        let Some(t) = self.tr() else { return 0 };
        let turn = crate::trace::turn_of(t, i);
        let mut top = turn;
        if let Some(p) = t.nodes[top].parent {
            top = p;
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::hash::DefaultHasher::new();
        if Some(top) == t.last_turn() || t.roots.last() == Some(&top) {
            (t.off, live).hash(&mut h);
        }
        let subs = t.subs_in(turn);
        if !subs.is_empty() {
            (live, subs.iter().filter_map(|&k| t.sub_of(k)).map(|s| s.t.grown()).sum::<u64>()).hash(&mut h);
        }
        t.nodes[turn].rolled.hash(&mut h);
        h.finish()
    }
}

impl App {
    /// `Z`: the highlighted session's record (see `trace`)
    pub(super) fn open_trace(&mut self) {
        let Some(s) = self.current().filter(|s| !s.blank) else { return };
        // Subagent row: open its session's record, and once read go straight into its level (Esc goes back to the turn that started it)
        let (name, goto) = match &s.subagent {
            Some(a) => (s.family().to_string(), Some(a.id.clone())),
            None => (s.name.clone(), None),
        };
        self.reader = None;
        self.trace = Some(TraceView { goto, ..TraceView::new(name) });
    }

    /// Whether to read the record in the background: just opened / session switched. Gives the run loop that session
    pub fn trace_request(&mut self) -> Option<Session> {
        let v = self.trace.as_mut()?;
        if !std::mem::take(&mut v.want) {
            return None;
        }
        let s = self.all.iter().find(|s| s.name == v.name).cloned();
        if s.is_none() {
            v.loading = false;
            v.err = Some(t!("会话已不存在", "the session is gone").into());
        }
        s
    }

    /// The background read is done. Dropped if the session has changed since
    pub fn set_trace(&mut self, name: &str, r: Result<crate::trace::Trace, String>) {
        let Some(v) = self.trace.as_mut().filter(|v| v.name == name) else { return };
        v.loading = false;
        match r {
            Ok(t) => v.set(t),
            Err(e) => v.err = Some(e),
        }
    }

    /// Periodic refresh: if the record grew, read the new tail (usually a few KB, a few ms)
    pub fn trace_tail(&mut self) {
        if let Some(v) = self.trace.as_mut() {
            v.tail();
        }
    }

    /// Whether the record's session is running (a running turn and tools with no result yet count up to now)
    fn trace_live(&self) -> bool {
        self.trace.as_ref().is_some_and(|v| self.all.iter().any(|s| s.name == v.name && matches!(s.state, State::Running | State::Ask)))
    }

    /// e: view the selected item read-only in the editor — tools that read / change a file show the file itself, others are
    /// written to a temporary Markdown file
    fn trace_open_view(&mut self) {
        let live = self.trace_live();
        let Some(v) = self.trace.as_ref() else { return };
        let live = v.live(live, now_ms());
        let Some(t) = v.tr() else { return };
        let i = match &v.detail {
            Some(d) => d.node,
            None if v.in_steps => v.steps.sel,
            None => v.chat.sel,
        };
        if t.nodes.get(i).is_none() {
            return;
        }
        match crate::trace::view(t, i, live, now_ms()) {
            None => self.flash(t!("此行没有可查看的内容", "nothing to see on this row")),
            Some(crate::trace::View::File { path, line }) => self.external = Some(External::View { path, line, temp: false }),
            Some(crate::trace::View::Text { text, ext, line }) => match write_view(&crate::trace::where_of(t, i), &text, &ext) {
                Ok(path) => self.external = Some(External::View { path, line, temp: true }),
                Err(e) => self.flash(&tr!("无法写入临时文件:{e}", "can't write the temp file: {e}")),
            },
        }
    }

    pub(super) fn on_trace_key(&mut self, k: KeyEvent) {
        if k.code == KeyCode::Char('?')
            && !k.modifiers.contains(KeyModifiers::CONTROL)
            && let Some(v) = self.trace.as_ref()
        {
            let keys = trace_keys(v);
            return self.open_sheet(&keys);
        }
        // Ctrl combos aren't letters (^Z isn't z for output): ^L restarts in place (same as the list and billing panel), others are ignored
        if k.modifiers.contains(KeyModifiers::CONTROL) {
            if k.code == KeyCode::Char('l') {
                self.ask_relaunch();
            }
            return;
        }
        let Some(v) = self.trace.as_mut() else { return };
        if let Some(d) = v.detail.as_mut() {
            let page = d.view_h.saturating_sub(1).max(1);
            match k.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => v.detail = None,
                KeyCode::Char('e') => return self.trace_open_view(),
                KeyCode::Up | KeyCode::Char('k') => d.scroll = d.scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => d.scroll = d.scroll.saturating_add(1),
                KeyCode::PageUp | KeyCode::Char('b') => d.scroll = d.scroll.saturating_sub(page),
                KeyCode::PageDown | KeyCode::Char(' ') => d.scroll = d.scroll.saturating_add(page),
                KeyCode::Home | KeyCode::Char('g') => d.scroll = 0,
                KeyCode::End | KeyCode::Char('G') => d.scroll = usize::MAX, // clamped to the bottom when drawn
                KeyCode::Char(c @ ('n' | 'p')) => {
                    // Next / previous block in the same column
                    let b = v.cur();
                    let at = b.at().unwrap_or(0);
                    let to = if c == 'n' { b.items.get(at + 1) } else { at.checked_sub(1).and_then(|a| b.items.get(a)) };
                    if let Some(x) = to.map(|x| x.0) {
                        b.jump(x);
                        v.detail = Some(TraceDetail::new(x));
                    }
                }
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Char('q' | 'Z') => return self.trace = None,
            // In a subagent: back up a level
            // Entered straight from a subagent row in the list: Esc goes back to
            // the list (← / h still go up a level, to whoever started it)
            KeyCode::Esc if !v.in_steps && v.home > 0 && v.path.len() == v.home => return self.trace = None,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') if !v.in_steps && v.leave_sub() => return,
            KeyCode::Esc if !v.in_steps => return self.trace = None,
            KeyCode::Char('z') | KeyCode::Tab => return self.open_reader(),
            KeyCode::Char('x') => return self.open_diff(),
            KeyCode::Char('e') => return self.trace_open_view(),
            KeyCode::Char('J') => return self.trace_step(1),
            KeyCode::Char('K') => return self.trace_step(-1),
            _ => {}
        }
        if v.t.is_none() {
            return;
        }
        let page = v.cur().view_h.saturating_sub(1).max(1) as isize;
        let sel = v.cur().sel;
        let kind = v.tr().and_then(|t| t.nodes.get(sel)).map(|n| n.kind);
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => v.cur().go(false),
            KeyCode::Down | KeyCode::Char('j') => v.cur().go(true),
            KeyCode::PageUp | KeyCode::Char('b') => v.cur().scroll(-page),
            KeyCode::PageDown | KeyCode::Char(' ') => v.cur().scroll(page),
            KeyCode::Home | KeyCode::Char('g') => v.cur().home(),
            KeyCode::End | KeyCode::Char('G') => {
                v.cur().follow = true;
                if !v.in_steps {
                    v.steps_of = None;
                }
            }
            // Right side: ⏎ views this step full screen (one that started a subagent: go into its record); ← / Esc back to the conversation
            KeyCode::Enter if v.in_steps => {
                if !v.enter_sub(sel) {
                    v.detail = Some(TraceDetail::new(sel));
                }
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc if v.in_steps => v.in_steps = false,
            // Left side: ⏎ / → unfolds a copied block; ⏎ / → on a turn shows its steps
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') if kind == Some(crate::trace::Kind::Copied) => {
                if !v.open.remove(&sel) {
                    v.open.insert(sel);
                }
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') if !v.in_steps && kind == Some(crate::trace::Kind::Turn) => {
                v.in_steps = true
            }
            KeyCode::Char('o') if !v.in_steps => {
                let clipped = v.chat_cache.get(&sel).is_some_and(|c| c.clipped);
                let can = v.open.contains(&sel)
                    || kind == Some(crate::trace::Kind::Copied)
                    || (kind == Some(crate::trace::Kind::Turn) && clipped);
                if !can {
                    return;
                }
                if !v.open.remove(&sel) {
                    v.open.insert(sel);
                    v.chat.reveal = true;
                }
            }
            // Right side: o folds / unfolds this step, O the whole turn
            KeyCode::Char('o') => {
                let fold = v.steps_cache.as_ref().and_then(|c| c.blocks.iter().find(|x| x.node == sel)).is_some_and(|x| x.fold);
                if !fold {
                    return;
                }
                if !v.unfold.remove(&sel) {
                    v.unfold.insert(sel);
                    v.steps.reveal = true;
                }
                v.unfold_gen += 1;
            }
            KeyCode::Char('O') if v.in_steps => {
                let folds: Vec<usize> =
                    v.steps_cache.as_ref().map_or(vec![], |c| c.blocks.iter().filter(|x| x.fold).map(|x| x.node).collect());
                if folds.iter().any(|x| v.unfold.contains(x)) {
                    folds.iter().for_each(|x| {
                        v.unfold.remove(x);
                    });
                } else {
                    v.unfold.extend(folds);
                }
                v.unfold_gen += 1;
            }
            KeyCode::Char('O') => {
                if v.open.is_empty() {
                    let all: Vec<usize> = v.tr().map_or(vec![], |t| {
                        (0..t.nodes.len())
                            .filter(|&x| matches!(t.nodes[x].kind, crate::trace::Kind::Turn | crate::trace::Kind::Copied))
                            .collect()
                    });
                    v.open.extend(all);
                } else {
                    v.open.clear();
                }
            }
            KeyCode::Char(c @ ('n' | 'p')) => {
                v.turn_step(c == 'n');
            }
            _ => {}
        }
    }

    /// J / K: switch to the next / previous session in the list and keep viewing records
    fn trace_step(&mut self, d: isize) {
        let cur = self.list.selected().unwrap_or(0) as isize;
        let next = cur + d;
        if next < 0 || next >= self.view.len() as isize {
            return;
        }
        self.select(Some(next as usize));
        self.open_trace();
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Record: split into two panes only this wide (the conversation on the left and the steps on the right each need room for a sentence)
const TRACE_SPLIT: usize = 90;

/// Max lines of a turn shown at first in the conversation (what you said / what Claude said); `o` shows the full text
const ASK_LINES: usize = 6;

const REPLY_LINES: usize = 12;

/// Z session record: full screen, a title line on top (turns · how long it worked · how much it cost); on desktop the conversation left,
/// the selected turn's steps right; on a phone the whole screen is one side; ⏎ views one step full screen; keys at the bottom
pub(super) fn draw_trace(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let (w, h) = (area.width as usize, body.height as usize);
    let live = app.trace_live();
    let now = now_ms();
    let msg = app.msg.as_ref().filter(|(_, t)| t.elapsed() < app.msg_ttl).map(|(m, _)| m.clone());
    let Some(v) = app.trace.as_mut() else { return };
    // Theme changed (settings page preview, theme file edited): re-lay everything already laid out
    let pal = theme::pal() as *const theme::Pal as usize;
    if v.pal != pal {
        v.pal = pal;
        v.chat_cache.clear();
        v.steps_cache = None;
        for lv in &mut v.saved {
            lv.chat_cache.clear();
            lv.steps_cache = None;
        }
        if let Some(d) = v.detail.as_mut() {
            d.lines.clear();
            d.wrap_w = 0;
        }
    }
    let s = app.all.iter().find(|s| s.name == v.name);
    let ic = s.map_or(("·", dim()), session_icon);
    let mut title = s.map_or(v.name.as_str(), |s| s.title.as_str()).to_string();
    // Inside a subagent: the title says who it is; running or not goes by its own
    let live = v.live(live, now);
    if let (Some(t), Some((&tool, up))) = (v.t.as_ref(), v.path.split_last())
        && let Some(sub) = t.at(up).sub_of(tool)
    {
        title = tr!(
            "子代理 · {}",
            "Subagent · {}",
            [sub.kind.as_str(), sub.desc.as_str()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ")
        );
    }
    let ic = if v.path.is_empty() {
        ic
    } else if live {
        ("●", Style::new().fg(theme::pal().green).add_modifier(Modifier::BOLD))
    } else {
        ("✓", dim())
    };
    v.wide = w >= TRACE_SPLIT;
    let foot_line = |groups: &[Vec<MenuItem>], action: Option<usize>| match &msg {
        Some(m) => Line::styled(m.clone(), theme::warn()).centered(),
        None => page_bar(groups, action, w),
    };
    // One step full screen
    if let (Some(d), Some(t)) = (v.detail.as_mut(), v.t.as_ref().map(|t| t.at(&v.path))) {
        if d.lines.is_empty() {
            d.lines = crate::trace::detail(t, d.node, live, now);
        }
        if d.wrap_w != w {
            d.wrapped = crate::trace::wrap(&d.lines, w.saturating_sub(3).max(1));
            d.wrap_w = w;
        }
        d.view_h = h;
        d.scroll = d.scroll.min(d.wrapped.len().saturating_sub(h));
        let n = &t.nodes[d.node];
        let dic = match n.state {
            crate::trace::St::Err | crate::trace::St::Denied => ("✕", Style::new().fg(theme::pal().red).add_modifier(Modifier::BOLD)),
            _ if t.running(d.node, live) => ("●", Style::new().fg(theme::pal().green).add_modifier(Modifier::BOLD)),
            _ => ("✓", dim()),
        };
        let pos = if d.wrapped.len() > h {
            tr!("{}/{} 行", "{}/{} lines", (d.scroll + h).min(d.wrapped.len()), d.wrapped.len())
        } else {
            clip(&title, 24).trim_end().to_string()
        };
        f.render_widget(Paragraph::new(title_rule(dic, &crate::trace::where_of(t, d.node), vec![Span::styled(pos, dim())], w)), head);
        let rows: Vec<Line> = d.wrapped.iter().skip(d.scroll).take(h).map(|l| pad_line(l, "  ")).collect();
        f.render_widget(Paragraph::new(rows), body);
        f.render_widget(Paragraph::new(foot_line(&trace_bar(v), None)), foot);
        return;
    }
    // Viewing steps on a phone: the top says which turn
    let phone_steps = !v.wide && v.in_steps;
    let head_t = v.t.as_ref().map(|t| t.at(&v.path));
    let title = if v.path.is_empty() { tr!("记录 · {title}", "Record · {title}") } else { title };
    let (label, right) = match (head_t, v.loading) {
        (_, true) => (title.clone(), t!("读取中…", "loading…").to_string()),
        (None, false) => (title.clone(), String::new()),
        (Some(t), false) if phone_steps && t.nodes.get(v.chat.sel).is_some() => {
            let i = v.chat.sel;
            let running = t.running(i, live);
            let s = t.sum(i);
            let at = crate::trace::where_of(t, i);
            let at = if v.path.is_empty() { at } else { tr!("子代理 · {at}", "Subagent · {at}") };
            (at, format!("{} · {}", crate::trace::fmt_dur(t.dur(i, running, now)), crate::trace::fmt_cost(s.cost)))
        }
        (Some(t), false) => {
            let (turns, dur, s) = t.total(live, now);
            let unit = noun(turns as u64, "turn", "turns");
            let right = if w >= 60 {
                tr!("{turns} 轮 · {} · {}", "{turns} {unit} · {} · {}", crate::trace::fmt_dur(dur), crate::trace::fmt_cost(s.cost))
            } else {
                tr!("{turns} 轮 · {}", "{turns} {unit} · {}", crate::trace::fmt_cost(s.cost))
            };
            (title.clone(), right)
        }
    };
    f.render_widget(Paragraph::new(title_rule(ic, &label, vec![Span::styled(right, dim())], w)), head);
    match (v.t.as_ref().map(|t| t.at(&v.path)), &v.err) {
        (_, Some(e)) => f.render_widget(Paragraph::new(format!(" {e}")).style(dim()), body),
        (None, None) => f.render_widget(Paragraph::new(t!(" 正在读取对话记录…", " reading the conversation record…")).style(dim()), body),
        (Some(t), None) if t.roots.is_empty() => {
            f.render_widget(Paragraph::new(t!(" 暂无对话", " no conversation yet")).style(dim()), body)
        }
        (Some(_), None) if v.wide => {
            let lw = (w / 2).clamp(34, 80) as u16;
            let [left, sep, right] = Layout::horizontal([Constraint::Length(lw), Constraint::Length(1), Constraint::Fill(1)]).areas(body);
            v.split = left.x + left.width;
            draw_chat(f, v, left, live, now);
            f.render_widget(Paragraph::new(vec![Line::styled("│", theme::faint()); h]), sep);
            draw_steps(f, v, right, live, now);
        }
        (Some(_), None) => {
            // Phone: lay out the conversation first (the steps are for the turn selected in it), then draw the side being viewed
            if v.in_steps {
                lay_chat(v, w, h, live, now);
                draw_steps(f, v, body, live, now);
            } else {
                draw_chat(f, v, body, live, now);
            }
        }
    }
    f.render_widget(Paragraph::new(foot_line(&trace_bar(v), Some(0))), foot);
}

/// The trace page's bottom bar for where it is: one step full screen, the two panes (the conversation or its steps), or on a phone
/// the conversation or the steps
fn trace_bar(v: &TraceView) -> Vec<Vec<MenuItem<'static>>> {
    if v.detail.is_some() {
        return vec![
            vec![
                ("↑↓", t!("滚动", "scroll"), 2),
                (t!("空格 b", "Space b"), t!("翻页", "page"), 3),
                ("n p", t!("上 / 下一项", "prev / next"), 1),
            ],
            vec![("Esc", t!("返回", "back"), 0)],
        ];
    }
    match (v.wide, v.in_steps) {
        (true, false) => vec![
            vec![("↑↓", t!("移动", "move"), 3), ("→", t!("查看过程", "steps"), 0), ("o", t!("全文", "full text"), 4)],
            vec![("n p", t!("切换轮次", "turn"), 2), ("J K", t!("切换会话", "session"), 5), ("Tab", t!("输出", "output"), 6)],
            vec![("Esc", t!("返回", "back"), 0)],
        ],
        (true, true) => vec![
            vec![("↑↓", t!("移动", "move"), 3), ("o", t!("展开", "unfold"), 0), ("⏎", t!("全屏查看", "full screen"), 1)],
            vec![("←", t!("返回对话", "conversation"), 2), ("n p", t!("切换轮次", "turn"), 4), ("J K", t!("切换会话", "session"), 5)],
            vec![("Esc", t!("返回", "back"), 0)],
        ],
        (false, false) => vec![
            vec![("⏎", t!("查看过程", "steps"), 0), ("o", t!("全文", "full text"), 3)],
            vec![("n p", t!("切换轮次", "turn"), 1), (t!("空格 b", "Space b"), t!("翻页", "page"), 2)],
            vec![("Esc", t!("返回", "back"), 0)],
        ],
        (false, true) => vec![
            vec![("o", t!("展开", "unfold"), 0), ("⏎", t!("详情", "details"), 1)],
            vec![("n p", t!("切换轮次", "turn"), 2), (t!("空格 b", "Space b"), t!("翻页", "page"), 3)],
            vec![("Esc", t!("返回", "back"), 0)],
        ],
    }
}

/// `?` on the trace page: the bottom bar's keys, then the ones it has no room for
fn trace_keys(v: &TraceView) -> Vec<crate::sheet::Key> {
    use crate::sheet::{key, more};
    let mut keys: Vec<crate::sheet::Key> = trace_bar(v).into_iter().flatten().map(|(k, w, _)| key(k, w)).collect();
    let has = |keys: &[crate::sheet::Key], k: &str| keys.iter().any(|x| x.key == k);
    let all = if v.in_steps { t!("全部展开", "unfold all") } else { t!("全部全文", "full text, all") };
    let extra = [
        ("O", all),
        (t!("空格 b", "Space b"), t!("翻页", "page")),
        ("e", t!("编辑器", "editor")),
        ("J K", t!("切换会话", "session")),
        ("z", t!("输出", "output")),
        ("x", t!("改动", "diff")),
        ("g G", t!("顶部 / 底部", "top / bottom")),
    ];
    for (k, w) in extra {
        if !has(&keys, k) && (v.detail.is_none() || k == "e" || k == "g G") {
            keys.push(more(k, w));
        }
    }
    keys
}

/// How many cells to indent a wrapped line
fn pad_line(l: &Styled, pad: &'static str) -> Line<'static> {
    let mut line = ansi::to_line(l);
    line.spans.insert(0, Span::raw(pad));
    line
}

/// Keep only the first max lines when there are more, the last saying how many more
fn clip_lines(mut ls: Vec<Styled>, max: usize, pad: &str) -> Vec<Styled> {
    if ls.len() > max {
        let more = ls.len() - max;
        ls.truncate(max);
        let (k, w) = (crate::trace::thousands(more as u64), noun(more as u64, "line", "lines"));
        ls.push(vec![crate::trace::seg(tr!("{pad}… 还有 {k} 行", "{pad}… {k} more {w}"), theme::faint())]);
    }
    ls
}

/// Lay out the conversation on the left: one block per item from the top — a turn is what you said, what Claude said last, a line of
/// numbers; compaction one line; the copied-over block one line (unfolded, its turns follow it)
fn lay_chat(v: &mut TraceView, w: usize, h: usize, live: bool, now: i64) -> Vec<Vec<Line<'static>>> {
    let Some(t) = v.t.as_ref().map(|t| t.at(&v.path)) else { return vec![] };
    // One cell for the vertical bar on the left, one blank, one cell left on the right
    let cw = w.saturating_sub(3).max(1);
    let mut tops = vec![];
    for &r in &t.roots {
        tops.push(r);
        if t.nodes[r].kind == crate::trace::Kind::Copied && v.open.contains(&r) {
            tops.extend(t.nodes[r].kids.iter().copied());
        }
    }
    // First lay out without color and measure each block's height (cached if laid out before), decide which line to start from, then color
    // the visible ones and turn them into screen lines
    let mut sizes = vec![];
    for &i in &tops {
        let len = if t.nodes[i].kind == crate::trace::Kind::Turn {
            let full = v.open.contains(&i);
            let ver = v.version(i, live);
            if !matches!(v.chat_cache.get(&i), Some(c) if c.w == cw && c.ver == ver && c.full == full) {
                let (lines, clipped) = crate::hl::plain(|| chat_lines(t, i, cw, full, live, now));
                v.chat_cache.insert(i, ChatLaid { w: cw, ver, full, lines, clipped, colored: 0 });
            }
            v.chat_cache[&i].lines.len()
        } else {
            2
        };
        sizes.push((i, len));
    }
    v.chat.lay(t, &sizes, h);
    let (top, end) = (v.chat.top, v.chat.top + h);
    let mut moved = false;
    for (&i, &(_, s, e)) in tops.iter().zip(&v.chat.items) {
        if e <= top || s >= end {
            continue;
        }
        // Color down to the bottom of the screen (again once more shows)
        let need = end.min(e) - s;
        if let Some(c) = v.chat_cache.get_mut(&i).filter(|c| c.colored < need) {
            let (lines, _) = crate::hl::upto(need, || chat_lines(t, i, cw, c.full, live, now));
            moved |= lines.len() != c.lines.len();
            c.lines = lines;
            c.colored = need;
        }
    }
    if moved {
        // Coloring shouldn't change the line count; if it really did, reposition by the new one
        let sizes: Vec<(usize, usize)> = tops.iter().map(|&i| (i, v.chat_cache.get(&i).map_or(2, |c| c.lines.len()))).collect();
        v.chat.lay(t, &sizes, h);
    }
    let (top, end) = (v.chat.top, v.chat.top + h);
    tops.iter()
        .zip(&v.chat.items)
        .map(|(&i, &(_, s, e))| match () {
            _ if e <= top || s >= end => vec![],
            _ if t.nodes[i].kind == crate::trace::Kind::Turn => v.chat_cache[&i].lines.iter().map(|l| pad_line(l, " ")).collect(),
            _ => vec![crate::trace::row(t, i, v.open.contains(&i), w.saturating_sub(1)), Line::raw("")],
        })
        .collect()
}

/// One turn of the conversation as lines: what you said, what Claude said last (folded, each shows its first few lines), a line of numbers;
/// second value: whether folding clipped anything
fn chat_lines(t: &crate::trace::Trace, i: usize, cw: usize, full: bool, live: bool, now: i64) -> (Vec<Styled>, bool) {
    let c = crate::trace::chat(t, i, live, now);
    let (ask_max, reply_max) = if full { (usize::MAX, usize::MAX) } else { (ASK_LINES, REPLY_LINES) };
    let (asked, reply) = (crate::trace::wrap(&c.asked, cw), crate::trace::wrap(&c.reply, cw));
    let clipped = asked.len() > ASK_LINES || reply.len() > REPLY_LINES;
    let mut ls = clip_lines(asked, ask_max, "    ");
    if !reply.is_empty() {
        ls.push(vec![]);
        ls.extend(clip_lines(reply, reply_max, "  "));
    }
    ls.extend(crate::trace::wrap(&[c.foot], cw));
    ls.push(vec![]);
    (ls, clipped)
}

/// Lay out the steps on the right: the turn selected in the conversation (a new turn starts from the top; following the latest sticks to
/// the bottom). Like the left: lay out without color first, then color the visible blocks
fn lay_steps(v: &mut TraceView, w: usize, h: usize, live: bool, now: i64) -> Vec<Vec<Line<'static>>> {
    let Some(t) = v.t.as_ref().map(|t| t.at(&v.path)) else { return vec![] };
    let cur = v.chat.sel;
    if t.nodes.get(cur).is_none() {
        return vec![];
    }
    if v.steps_of != Some(cur) {
        v.steps = Blocks { sel: cur, follow: v.chat.follow, ..Blocks::default() };
        v.steps_of = Some(cur);
    }
    let cw = w.saturating_sub(3).max(1);
    let (ver, folds) = (v.version(cur, live), v.unfold_gen);
    let turn = t.nodes[cur].kind == crate::trace::Kind::Turn;
    let unfold = &v.unfold;
    let open = |x: usize| unfold.contains(&x);
    // This item isn't a turn (the copied-over block, compaction): the whole page of details as one block
    let build = |k: usize| {
        if turn {
            crate::trace::step(t, cur, k, live, now, &open)
        } else {
            Some(crate::trace::Step { node: cur, paras: crate::trace::detail(t, cur, live, now), fold: false })
        }
    };
    if !matches!(&v.steps_cache, Some(c) if c.item == cur && c.w == cw && c.ver == ver && c.folds == folds) {
        // Same turn, same width: reuse unchanged blocks (see trace::step_sig), lay out only the changed ones
        let mut old: HashMap<usize, StepLaid> = match v.steps_cache.take() {
            Some(c) if c.item == cur && c.w == cw => c.blocks.into_iter().map(|b| (b.node, b)).collect(),
            _ => HashMap::new(),
        };
        let blocks = if turn {
            let list = crate::trace::step_nodes(t, cur);
            (0..list.len())
                .filter_map(|k| {
                    let sig = crate::trace::step_sig(t, cur, &list, k, live, now, &open);
                    if let Some(b) = old.remove(&list[k]).filter(|b| b.sig == sig) {
                        return Some(b);
                    }
                    let s = crate::hl::plain(|| build(k))?;
                    // Folded thinking is clipped by line count: only clipped at this width is there anything to unfold (a phone clips more)
                    let (lines, clipped) = crate::trace::wrap_clipped(&s.paras, cw);
                    Some(StepLaid { node: s.node, lines, fold: s.fold || clipped, colored: 0, sig })
                })
                .collect()
        } else {
            crate::hl::plain(|| build(0))
                .into_iter()
                .map(|s| StepLaid { node: s.node, lines: crate::trace::wrap(&s.paras, cw), fold: s.fold, colored: 0, sig: 0 })
                .collect()
        };
        v.steps_cache = Some(StepsLaid { item: cur, w: cw, ver, folds, blocks });
    }
    let blocks = &mut v.steps_cache.as_mut().unwrap().blocks;
    let sizes: Vec<(usize, usize)> = blocks.iter().map(|b| (b.node, b.lines.len())).collect();
    v.steps.lay(t, &sizes, h);
    let (top, end) = (v.steps.top, v.steps.top + h);
    let mut moved = false;
    for (k, &(_, s, e)) in v.steps.items.iter().enumerate() {
        let need = end.min(e).saturating_sub(s);
        if e <= top || s >= end || blocks[k].colored >= need {
            continue;
        }
        if let Some(st) = crate::hl::upto(need, || build(k)) {
            let lines = crate::trace::wrap(&st.paras, cw);
            moved |= lines.len() != blocks[k].lines.len();
            blocks[k].lines = lines;
        }
        blocks[k].colored = need;
    }
    if moved {
        let sizes: Vec<(usize, usize)> = blocks.iter().map(|b| (b.node, b.lines.len())).collect();
        v.steps.lay(t, &sizes, h);
    }
    let (top, end) = (v.steps.top, v.steps.top + h);
    blocks
        .iter()
        .zip(&v.steps.items)
        .map(|(b, &(_, s, e))| if e <= top || s >= end { vec![] } else { b.lines.iter().map(|l| pad_line(l, " ")).collect() })
        .collect()
}

/// Draw a column of blocks: a vertical bar left of the selected block — accent color in the column with focus, its first line on light
/// purple; the conversation without focus gets a faint bar (the steps on the right are that turn's), the steps without focus get none
fn draw_blocks(f: &mut Frame, area: Rect, b: &Blocks, blocks: Vec<Vec<Line<'static>>>, focus: bool, faint_bar: bool) {
    let (top, h) = (b.top, area.height as usize);
    let k = b.at();
    let mut out = vec![];
    for (bi, ls) in blocks.into_iter().enumerate() {
        let Some(&(_, bs, be)) = b.items.get(bi) else { break };
        if be <= top || bs >= top + h {
            continue;
        }
        let bar = match (Some(bi) == k, focus) {
            (true, true) => Span::styled("▌", theme::accent()),
            (true, false) if faint_bar => Span::styled("▏", theme::faint()),
            _ => Span::raw(" "),
        };
        for (j, mut line) in ls.into_iter().enumerate() {
            if (top..top + h).contains(&(bs + j)) {
                line.spans.insert(0, bar.clone());
                out.push(line);
            }
        }
    }
    f.render_widget(Paragraph::new(out), area);
    if focus && let Some(s) = k.map(|k| b.items[k].1).filter(|s| (top..top + h).contains(s)) {
        f.buffer_mut()
            .set_style(Rect { x: area.x, y: area.y + (s - top) as u16, width: area.width, height: 1 }, Style::new().bg(theme::pal().sel));
    }
    let below = b.total().saturating_sub(top + h);
    if below > 0 && !b.follow {
        below_tag(f, area, below);
    }
}

/// More below: the bottom row of `area` gives way to a tag at its right saying how many lines are left (that row counted
/// too), rather than covering the end of a line
pub(super) fn below_tag(f: &mut Frame, area: Rect, below: usize) {
    let n = below + 1;
    let w = noun(n as u64, "line", "lines");
    let tag = tr!(" ↓ {n} 行 ", " ↓ {n} {w} ");
    let tw = tag.width() as u16;
    if area.height < 2 || area.width <= tw + 2 {
        return;
    }
    let row = Rect { y: area.y + area.height - 1, height: 1, ..area };
    f.render_widget(ratatui::widgets::Clear, row);
    let r = Rect { x: row.x + row.width - tw, width: tw, ..row };
    f.render_widget(Paragraph::new(Line::styled(tag, dim())).style(Style::new().bg(theme::pal().sel)), r);
}

fn draw_chat(f: &mut Frame, v: &mut TraceView, area: Rect, live: bool, now: i64) {
    let blocks = lay_chat(v, area.width as usize, area.height as usize, live, now);
    draw_blocks(f, area, &v.chat, blocks, !v.in_steps, true);
}

fn draw_steps(f: &mut Frame, v: &mut TraceView, area: Rect, live: bool, now: i64) {
    let blocks = lay_steps(v, area.width as usize, area.height as usize, live, now);
    draw_blocks(f, area, &v.steps, blocks, v.in_steps, false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{fixture, press, reader_rows, select, shows, to_scope, with_agents};

    /// Open the record of 「跑」 and feed it the test transcript (no background thread)
    fn trace_app(name: &str) -> (App, std::path::PathBuf) {
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('Z'));
        assert!(app.trace.is_some());
        let s = app.trace_request().unwrap();
        assert_eq!(s.name, "跑");
        assert!(app.trace_request().is_none(), "只读一次");
        let (t, p) = aca_records::trace_claude::fixtures::fixture(&format!("ui-{name}"));
        app.set_trace("跑", Ok(t));
        // Keys go by the last draw: in real use it's drawn once after loading before any key comes in
        reader_rows(&mut app, 120, 30);
        (app, p)
    }

    /// The two desktop panes: the left (list) and the right (right of the divider)
    fn trace_panes(app: &mut App, w: u16, h: u16) -> (Vec<String>, Vec<String>) {
        let rows = reader_rows(app, w, h);
        let split = |r: &String| r.split_once('│').map(|(a, b)| (a.to_string(), b.to_string()));
        let body: Vec<(String, String)> = rows[1..rows.len() - 1].iter().filter_map(split).collect();
        (body.iter().map(|x| x.0.clone()).collect(), body.iter().map(|x| x.1.clone()).collect())
    }

    /// What's selected in the pane taking keys: a turn gives its prompt's first line, a call the first thing it said, a tool its
    /// summary (or its name if none)
    fn trace_sel(app: &App) -> String {
        let v = app.trace.as_ref().unwrap();
        let t = v.tr().unwrap();
        let n = &t.nodes[if v.in_steps { v.steps.sel } else { v.chat.sel }];
        if n.label.is_empty() { n.name.clone() } else { n.label.clone() }
    }

    #[test]
    fn enter_on_a_subagent_step_opens_its_own_record_and_esc_comes_back_to_that_step() {
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('Z'));
        let _ = app.trace_request();
        let (p, _, later) = aca_records::trace_claude::fixtures::with_subs("ui-subs");
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        for l in &later {
            use std::io::Write;
            writeln!(f, "{l}").unwrap();
        }
        app.set_trace("跑", Ok(crate::trace::Trace::open_born(p, Agent::Claude, 0).unwrap()));
        reader_rows(&mut app, 120, 30);
        // In the first turn's steps, stop on the step that starts Explore
        press(&mut app, KeyCode::Char('p'));
        reader_rows(&mut app, 120, 30);
        press(&mut app, KeyCode::Right);
        reader_rows(&mut app, 120, 30);
        for _ in 0..6 {
            if trace_sel(&app) == "Explore · 找 width" {
                break;
            }
            press(&mut app, KeyCode::Down);
            reader_rows(&mut app, 120, 30);
        }
        assert_eq!(trace_sel(&app), "Explore · 找 width");
        let (_, right) = trace_panes(&mut app, 120, 30);
        shows(&right, &["▸ Agent Explore · 找 width", "3 调用 · $", "找到 4 处:", "… 还有 2 行"]);
        // ⏎: its own record, two panes as well; the left is the task it was given and what it said last, the right its steps
        // (where it starts another one)
        press(&mut app, KeyCode::Enter);
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows[0].contains("子代理 · Explore · 找 width"), "{}", rows[0]);
        let (left, right) = trace_panes(&mut app, 120, 30);
        shows(&left, &["任务  找出所有 width( 的调用", "Claude  找到 4 处:"]);
        shows(&right, &["· Agent general-purpose · 细看 ui.rs", "ui.rs 里有 2 处", "Bash  rg -n 'width\\('"]);
        // One level deeper, then Esc back level by level, to the step that started it
        for _ in 0..6 {
            if trace_sel(&app) == "general-purpose · 细看 ui.rs" {
                break;
            }
            press(&mut app, KeyCode::Up);
            reader_rows(&mut app, 120, 30);
        }
        press(&mut app, KeyCode::Enter);
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows[0].contains("子代理 · general-purpose · 细看 ui.rs"), "{}", rows[0]);
        for _ in 0..2 {
            press(&mut app, KeyCode::Esc);
            reader_rows(&mut app, 120, 30);
            press(&mut app, KeyCode::Esc);
            reader_rows(&mut app, 120, 30);
        }
        let v = app.trace.as_ref().unwrap();
        assert!(v.path.is_empty() && v.in_steps);
        assert_eq!(trace_sel(&app), "Explore · 找 width");
        // Phone width: entering a subagent also shows its steps full screen
        press(&mut app, KeyCode::Enter);
        let rows = reader_rows(&mut app, 46, 30);
        assert!(rows.iter().any(|r| r.contains("· Agent general-purpose")), "{rows:#?}");
    }

    #[test]
    fn enter_on_a_subagent_row_opens_the_record_at_that_subagent() {
        let mut app = App::new(String::new(), with_agents());
        to_scope(&mut app, Scope::Week);
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('l'));
        // ⏎: doesn't enter a session (it has no pane), opens the session's record
        press(&mut app, KeyCode::Enter);
        assert!(app.attach.is_none() && app.start.is_none());
        assert_eq!(app.trace_request().map(|s| s.title), Some("跑".to_string()));
        let (p, _, _) = aca_records::trace_claude::fixtures::with_subs("ui-agent-row");
        app.set_trace("跑", Ok(crate::trace::Trace::open_born(p.clone(), Agent::Claude, 0).unwrap()));
        // Once loaded, it's at that subagent's level
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows[0].contains("子代理 · Explore · 找 width"), "{}", rows[0]);
        assert_eq!(app.trace.as_ref().unwrap().path.len(), 1);
        let (left, right) = trace_panes(&mut app, 120, 30);
        shows(&left, &["任务  找出所有 width( 的调用", "Claude  找到 4 处:"]);
        shows(&right, &["Bash  rg -n 'width\\('"]);
        // One Esc goes back to the list, highlight still on that row
        press(&mut app, KeyCode::Esc);
        assert!(app.trace.is_none());
        assert_eq!(app.current().unwrap().title, "Explore · 找 width");
        // The nested one: two levels in at once. Z is the same as ⏎
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('Z'));
        let _ = app.trace_request();
        app.set_trace("跑", Ok(crate::trace::Trace::open_born(p, Agent::Claude, 0).unwrap()));
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows[0].contains("子代理 · general-purpose · 细看 ui.rs"), "{}", rows[0]);
        assert_eq!(app.trace.as_ref().unwrap().path.len(), 2);
        // ← up one level: the subagent that started it (on its steps on the right). Esc from there goes the usual way: first to the
        // left, then out to the session's own record, then back to the list
        press(&mut app, KeyCode::Left);
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows[0].contains("子代理 · Explore · 找 width"), "{}", rows[0]);
        for depth in [1, 0] {
            press(&mut app, KeyCode::Esc);
            reader_rows(&mut app, 120, 30);
            assert_eq!(app.trace.as_ref().map(|v| v.path.len()), Some(depth));
        }
        press(&mut app, KeyCode::Esc);
        assert!(app.trace.is_none());
    }

    #[test]
    fn shift_z_shows_the_chat_on_the_left_and_the_turns_steps_on_the_right() {
        let (mut app, _) = trace_app("shows");
        let (left, right) = trace_panes(&mut app, 120, 30);
        // That turn in the record is in the 01:00 UTC minute; drawn in local time (09:00 in UTC+8)
        let numbers = format!("{} · 20.0s · $0.03", crate::trace::clock(3_600_000, aca_records::usage::local_offset(), false));
        // The left is the conversation: what you said, what Claude said last (Markdown laid out), a line of numbers; what it said in
        // between isn't here
        shows(
            &left,
            &[
                "上一段 · 1 轮 · 不计",
                "你  看看两栏为什么对不齐",
                "Claude  改好了:len() 数的是字节",
                "• 换成按显示宽度算的 width()",
                &numbers,
                "── 压缩 97k → 12k · 21.0s",
                "你  再试一次",
                "✕ 出错",
                "你  跑一下测试",
                "● 运行中",
            ],
        );
        assert!(!left.iter().any(|r| r.contains("先看 bill.rs")), "中间说的在右边:{left:#?}");
        // The right is the steps of the selected turn (following the latest = the running turn)
        shows(
            &right,
            &["1 调用 · 2 工具", "── 4.1s", "▸ Read  src/usage.rs", "5 行", "· Bash  cargo build --release", "运行中 · ", "● 运行中…"],
        );
        shows(&reader_rows(&mut app, 120, 30), &["记录 · 跑", "3 轮", "→ 查看过程"]);
        // Keys on the left: the first line of the selected block on the left gets a pale purple background, the right gets none
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer();
        let rows = reader_rows(&mut app, 120, 30);
        let y = rows.iter().position(|r| r.contains("你  跑一下测试")).unwrap() as u16;
        assert_eq!(buf[(1, y)].bg, theme::pal().sel);
        assert!((0..30u16).all(|y| buf[(100, y)].bg != theme::pal().sel), "右边没在按,不铺底");
        for y in 0..30u16 {
            for x in 0..120u16 {
                let c = &buf[(x, y)];
                assert!(!c.modifier.contains(Modifier::DIM), "({x},{y}) 用了 DIM");
                assert!(!matches!(c.fg, Color::Yellow | Color::Gray | Color::White | Color::DarkGray), "({x},{y}) {:?}", c.fg);
            }
        }
        // Wrapped at every width, it all fits
        let v = app.trace.as_ref().unwrap();
        let tr = v.t.as_ref().unwrap();
        for w in [20, 34, 43, 60, 80] {
            for &r in tr.roots.iter().filter(|&&r| tr.nodes[r].kind == crate::trace::Kind::Turn) {
                let c = crate::trace::chat(tr, r, true, 0);
                let steps: Vec<crate::trace::Para> =
                    crate::trace::steps(tr, r, true, 0, &|_| false).into_iter().flat_map(|x| x.paras).collect();
                for l in [c.asked, c.reply, vec![c.foot], steps].iter().flat_map(|ps| crate::trace::wrap(ps, w)) {
                    assert!(crate::ansi::plain(&l).width() <= w, "{w} 宽折不下:{}", crate::ansi::plain(&l));
                }
            }
        }
        // On a phone the title and the cost still show
        shows(&reader_rows(&mut app, 46, 20), &["记录 · 跑", "3 轮 · $"]);
    }

    #[test]
    fn shift_z_keys_move_through_turns_step_into_the_process_and_back() {
        let (mut app, _) = trace_app("keys");
        // p: previous turn (doesn't stop on the compaction line)
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(trace_sel(&app), "再试一次");
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(trace_sel(&app), "看看两栏为什么对不齐");
        // The right switches to this turn's steps: each call's numbers, thinking, what it said in between, tools with their diffs and
        // the start of their output; the reply is on the left, only one line of it here
        let right = trace_panes(&mut app, 120, 40).1;
        shows(
            &right,
            &[
                "4 调用 · 3 工具",
                "── 4.0s · 1.1k · $0.01 ──",
                "思考 先确认是不是宽字符的问题",
                "先看 bill.rs 的列宽",
                "▸ Grep  width(",
                "src/bill.rs:12:",
                "… 还有 1 行",
                "▸ Edit  src/bill.rs",
                "+2 -2",
                "12 -     let w = s.len();",
                "12 +     let w = s.width();",
                "running 220 tests",
                "回复 改好了:len() 数的是字节",
            ],
        );
        assert!(!right.iter().any(|r| r.contains("• 测试全过了")), "回复全文在左边:{right:#?}");
        // → into the right pane, step by step; ⏎ shows this step full screen
        press(&mut app, KeyCode::Right);
        assert!(app.trace.as_ref().unwrap().in_steps);
        shows(&reader_rows(&mut app, 120, 20), &["← 返回对话", "Esc 返回 • ? 快捷键"]);
        press(&mut app, KeyCode::Down);
        assert_eq!(trace_sel(&app), "先看 bill.rs 的列宽");
        press(&mut app, KeyCode::Down);
        assert_eq!(trace_sel(&app), "width(");
        press(&mut app, KeyCode::Enter);
        shows(&reader_rows(&mut app, 100, 24), &["Grep · 第 1 轮", "── 输入 ──", "── 输出 ──", "src/ui.rs:90:"]);
        // n in full screen: the next step
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 100, 24), &["第 1 轮 · 第 2 次调用", " · 缓存写 100 · ", "── 调用了 ──", "▸ Edit  src/bill.rs"]);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        shows(
            &reader_rows(&mut app, 100, 24),
            &["Edit · 第 1 轮", "── 改动 ──", "11   fn col(s: &str)", "12 +     let w = s.width();", "┈┈┈┈┈", "41 +     pad(w.max(1))"],
        );
        press(&mut app, KeyCode::Esc);
        // The first call full screen: thinking first, then what it said, tokens listed separately
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        shows(
            &reader_rows(&mut app, 100, 30),
            &["第 1 轮 · 第 1 次调用", "claude-opus-5-5", "── 思考 ──", "中文一个字占两格", "── 回复 ──", "── 调用了 ──"],
        );
        press(&mut app, KeyCode::Esc);
        // n / p on the right change turns, staying on the right, from the top
        press(&mut app, KeyCode::Char('n'));
        assert!(app.trace.as_ref().unwrap().in_steps);
        assert_eq!(trace_sel(&app), "再试一次");
        shows(&trace_panes(&mut app, 120, 20).1, &["出错", "529 server_error"]);
        // ← / Esc back to the conversation; only another Esc goes back to the list
        press(&mut app, KeyCode::Esc);
        assert!(!app.trace.as_ref().unwrap().in_steps);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Left);
        assert!(app.trace.is_some() && !app.trace.as_ref().unwrap().in_steps);
        // Tab shows the output, Z comes back; Esc back to the list
        press(&mut app, KeyCode::Tab);
        assert!(app.trace.is_none() && app.reader.is_some());
        press(&mut app, KeyCode::Char('Z'));
        assert!(app.trace.is_some() && app.reader.is_none());
        press(&mut app, KeyCode::Esc);
        assert!(app.trace.is_none() && app.reader.is_none() && !app.quit);
    }

    #[test]
    fn shift_z_long_messages_show_their_head_and_o_shows_the_rest() {
        use aca_records::trace_claude::fixtures::{file, prompt, reply, turn_end};
        use serde_json::json;
        let ask: String = (1..=20).map(|i| format!("第 {i} 行问题\n")).collect();
        let answer: String = (1..=30).map(|i| format!("第 {i} 行回答\n")).collect();
        let p = file(
            "ui-long",
            &[
                prompt("u1", None, 0, &ask),
                reply("a1", "u1", 1000, "m1", json!({"type": "text", "text": answer}), 9, Some("end_turn")),
                turn_end("a1", 1100, 1000),
            ],
        );
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('Z'));
        let _ = app.trace_request();
        app.set_trace("跑", Ok(crate::trace::Trace::open_born(p, aca_records::agent::Agent::Claude, 0).unwrap()));
        let left = trace_panes(&mut app, 120, 40).0;
        shows(&left, &["你  第 1 行问题", "第 6 行问题", "… 还有 14 行", "Claude  第 1 行回答", "第 12 行回答", "… 还有 18 行"]);
        assert!(!left.iter().any(|r| r.contains("第 7 行问题")));
        press(&mut app, KeyCode::Char('o'));
        let rows = reader_rows(&mut app, 120, 80);
        shows(&rows, &["第 20 行问题", "第 30 行回答"]);
        assert!(!rows.iter().any(|r| r.contains("… 还有")));
    }

    #[test]
    fn shift_z_o_opens_and_closes_a_step_and_shift_o_the_whole_turn() {
        let (mut app, _) = trace_app("fold");
        // Keys go by the last draw: draw after each one, as in real use
        let key = |app: &mut App, k: KeyCode| {
            press(app, k);
            trace_panes(app, 120, 50)
        };
        key(&mut app, KeyCode::Char('p'));
        key(&mut app, KeyCode::Char('p'));
        // Left: a turn that's already in full, o says so
        key(&mut app, KeyCode::Char('o'));
        assert!(app.msg.is_none());
        // Edit selected on the right: folded, only the changed lines
        key(&mut app, KeyCode::Right);
        for _ in 0..4 {
            key(&mut app, KeyCode::Down);
        }
        assert_eq!(trace_sel(&app), "src/bill.rs");
        let right = trace_panes(&mut app, 120, 50).1;
        shows(&right, &["▸ Edit  src/bill.rs", "12 -     let w = s.len();"]);
        assert!(!right.iter().any(|r| r.contains("fn col(s: &str)")), "{right:#?}");
        // o: with context and a dotted line between the two hunks; o again folds it
        let right = key(&mut app, KeyCode::Char('o')).1;
        shows(&right, &["▾ Edit  src/bill.rs", "11   fn col(s: &str) -> usize {", "┈┈┈┈┈", "42   }"]);
        let right = key(&mut app, KeyCode::Char('o')).1;
        assert!(!right.iter().any(|r| r.contains("fn col(s: &str)")), "{right:#?}");
        // A step with nothing else (the call with only numbers): says so
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Char('o'));
        assert!(app.msg.is_none());
        // O: unfold the whole turn (all the thinking, every search hit, the whole command and all output), O again folds everything
        let right = key(&mut app, KeyCode::Char('O')).1;
        shows(
            &right,
            &[
                "中文一个字占两格",
                "▾ Grep",
                "src/ui.rs:90:",
                "▾ Edit",
                "fn col(s: &str)",
                "$ cargo test --release",
                "test result: ok. 220 passed",
            ],
        );
        let right = key(&mut app, KeyCode::Char('O')).1;
        shows(&right, &["▸ Grep", "▸ Edit", "▸ Bash", "… 还有 3 行"]);
        // Short thinking: in full even folded, flattened to one line
        shows(&right, &["思考 先确认是不是宽字符的问题。中文一个字占两格"]);
    }

    #[test]
    fn shift_z_e_opens_the_selected_step_read_only_in_the_editor() {
        let (mut app, _) = trace_app("view");
        let key = |app: &mut App, k: KeyCode| {
            press(app, k);
            trace_panes(app, 120, 50)
        };
        key(&mut app, KeyCode::Char('p'));
        key(&mut app, KeyCode::Char('p'));
        // Bash selected on the right (the record's files aren't on this machine): written to a temporary Markdown file, deleted
        // after viewing
        key(&mut app, KeyCode::Right);
        for _ in 0..6 {
            key(&mut app, KeyCode::Down);
        }
        assert_eq!(trace_sel(&app), "cargo test --release");
        key(&mut app, KeyCode::Char('e'));
        let Some(External::View { path, line: 1, temp: true }) = app.external.take() else { panic!("{:?}", app.msg) };
        assert!(path.ends_with("/aca-trace-view/Bash-第1轮.md"), "{path}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Bash · cargo test --release") && text.contains("test result: ok. 220 passed"), "{text}");
        let _ = std::fs::remove_file(&path);
        // e works in full screen too
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char('e'));
        assert!(matches!(app.external.take(), Some(External::View { temp: true, .. })));
        key(&mut app, KeyCode::Esc);
        // Left: a turn shows the whole turn; the compaction line has nothing to view
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('e'));
        let Some(External::View { path, .. }) = app.external.take() else { panic!() };
        assert!(std::fs::read_to_string(&path).unwrap().contains("## 你\n\n看看两栏为什么对不齐"));
        let _ = std::fs::remove_file(&path);
        key(&mut app, KeyCode::Char('n'));
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Char('e'));
        assert!(app.external.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "此行没有可查看的内容");
    }

    #[test]
    fn shift_z_colors_only_what_is_on_screen() {
        let (mut app, _) = trace_app("color");
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        // Short screen: the right shows only the top few blocks, the ones below aren't colored yet
        reader_rows(&mut app, 120, 12);
        let v = app.trace.as_ref().unwrap();
        let c = v.steps_cache.as_ref().unwrap();
        let (top, h) = (v.steps.top, v.steps.view_h);
        for (b, &(_, s, e)) in c.blocks.iter().zip(&v.steps.items) {
            let seen = e > top && s < top + h;
            assert_eq!(b.colored > 0, seen, "块 {} 在 {s}..{e},屏幕 {top}..{}", b.node, top + h);
        }
        // Tall screen: Edit is in view, and on screen let has the keyword color
        let mut term = Terminal::new(TestBackend::new(120, 50)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = reader_rows(&mut app, 120, 50);
        let y = rows.iter().position(|r| r.contains("12 +     let w = s.width();")).unwrap();
        let x = (0..120u16)
            .find(|&x| {
                buf[(x, y as u16)].symbol() == "l" && buf[(x + 1, y as u16)].symbol() == "e" && buf[(x + 2, y as u16)].symbol() == "t"
            })
            .unwrap();
        assert_eq!(buf[(x, y as u16)].fg, theme::pal().syn_kw);
        assert_eq!(buf[(x, y as u16)].bg, theme::pal().add_bg);
    }

    #[test]
    fn shift_z_ctrl_keys_are_not_letters_and_ctrl_l_restarts() {
        let (mut app, _) = trace_app("ctrl");
        // ^Z isn't z (view output), ^O isn't o
        app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(app.trace.is_some() && app.reader.is_none());
        assert!(app.trace.as_ref().unwrap().open.is_empty());
        app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert!(app.relaunch && app.quit);
    }

    #[test]
    fn shift_z_redraws_in_the_new_colors_after_a_theme_switch() {
        let (mut app, _) = trace_app("theme");
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        let kw_at = |app: &mut App| {
            let mut term = Terminal::new(TestBackend::new(120, 50)).unwrap();
            term.draw(|f| draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            let rows = reader_rows(app, 120, 50);
            let y = rows.iter().position(|r| r.contains("12 +     let w = s.width();")).unwrap() as u16;
            let x = (0..118u16)
                .find(|&x| buf[(x, y)].symbol() == "l" && buf[(x + 1, y)].symbol() == "e" && buf[(x + 2, y)].symbol() == "t")
                .unwrap();
            (buf[(x, y)].fg, buf[(x, y)].bg)
        };
        assert_eq!(kw_at(&mut app), (theme::pal().syn_kw, theme::pal().add_bg));
        // Previewing another theme on the settings page: already laid-out lines have the old colors and need laying out again
        let mut p = *theme::pal();
        (p.syn_kw, p.add_bg) = (Color::Rgb(1, 2, 3), Color::Rgb(4, 5, 6));
        theme::set(Box::leak(Box::new(p)));
        assert_eq!(kw_at(&mut app), (Color::Rgb(1, 2, 3), Color::Rgb(4, 5, 6)));
    }

    #[test]
    fn shift_z_each_side_scrolls_on_its_own() {
        let (mut app, _) = trace_app("scroll");
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        let (_, right) = trace_panes(&mut app, 120, 12);
        assert!(right.iter().any(|r| r.contains("↓") && r.contains("行")), "右边底下还有要说:{right:#?}");
        // Wheel: scrolls whichever side the mouse is on
        let wheel = |app: &mut App, column: u16| {
            app.on_mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column, row: 5, modifiers: KeyModifiers::NONE })
        };
        wheel(&mut app, 100);
        let v = app.trace.as_ref().unwrap();
        assert_eq!((v.steps.top, v.chat.top), (3, v.chat.top));
        let chat_top = v.chat.top;
        wheel(&mut app, 5);
        assert!(app.trace.as_ref().unwrap().chat.top > chat_top);
        // Turn changed: the right starts from the top
        press(&mut app, KeyCode::Char('n'));
        trace_panes(&mut app, 120, 12);
        assert_eq!(app.trace.as_ref().unwrap().steps.top, 0);
    }

    #[test]
    fn one_line_left_is_a_line_in_english() {
        let last = |n: usize| i18n::with(Lang::En, || clip_lines(vec![vec![]; n], 2, "").last().unwrap()[0].text.clone());
        assert_eq!((last(3), last(4)), ("… 1 more line".to_string(), "… 2 more lines".to_string()));
    }

    #[test]
    fn more_below_in_the_record_takes_the_bottom_row_rather_than_covering_a_line() {
        let (mut app, _) = trace_app("phone-more");
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        let rows = reader_rows(&mut app, 46, 8);
        assert!(rows.iter().any(|r| r.trim().starts_with("↓ ") && r.trim().ends_with('行')), "{rows:#?}");
    }

    #[test]
    fn shift_z_on_a_phone_is_the_chat_and_a_turns_steps_open_full_screen() {
        let (mut app, _) = trace_app("phone");
        // Just opened: the whole screen is the conversation, at the latest
        let rows = reader_rows(&mut app, 46, 16);
        shows(&rows, &["你  跑一下测试", "● 运行中", "⏎ 查看过程"]);
        assert!(!rows[1..rows.len() - 1].iter().any(|r| r.contains('│')), "手机上不分两栏:{rows:#?}");
        assert_eq!(trace_sel(&app), "跑一下测试");
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(trace_sel(&app), "看看两栏为什么对不齐");
        // A block taller than the screen: ↓ scrolls inside it first
        reader_rows(&mut app, 46, 8);
        let top = app.trace.as_ref().unwrap().chat.top;
        press(&mut app, KeyCode::Down);
        assert_eq!(trace_sel(&app), "看看两栏为什么对不齐");
        assert_eq!(app.trace.as_ref().unwrap().chat.top, top + 1);
        // ⏎ shows this turn's steps full screen, with the turn number on top
        press(&mut app, KeyCode::Enter);
        let rows = reader_rows(&mut app, 46, 30);
        shows(&rows, &["第 1 轮", "── 4.0s", "▸ Grep  width(", "▸ Edit  src/bill.rs", "o 展开", "Esc 返回 • ? 快捷键"]);
        assert!(!rows.iter().any(|r| r.contains("你  看看两栏")));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        shows(&reader_rows(&mut app, 46, 20), &["Grep · 第 1 轮", "── 输出 ──"]);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        shows(&reader_rows(&mut app, 46, 16), &["你  看看两栏为什么对不齐", "⏎ 查看过程"]);
        // G back to the latest
        press(&mut app, KeyCode::Char('G'));
        reader_rows(&mut app, 46, 16);
        assert_eq!(trace_sel(&app), "跑一下测试");
    }

    #[test]
    fn shift_z_follows_new_turns_while_it_sits_at_the_bottom() {
        use std::io::Write;
        let (mut app, p) = trace_app("follows");
        let add = |v: serde_json::Value| writeln!(std::fs::OpenOptions::new().append(true).open(&p).unwrap(), "{v}").unwrap();
        trace_panes(&mut app, 120, 20);
        add(aca_records::trace_claude::fixtures::done("r3", "a5", 50_000, "t3", false));
        add(aca_records::trace_claude::fixtures::prompt("u4", Some("r3"), 60_000, "下一句"));
        app.trace_tail();
        // A new turn arrives: the left selects it, stuck to the bottom; the right switches to its steps
        let (left, right) = trace_panes(&mut app, 120, 20);
        assert_eq!(trace_sel(&app), "下一句");
        shows(&left, &["你  下一句"]);
        assert!(!right.iter().any(|r| r.contains("cargo build")), "{right:#?}");
        shows(&reader_rows(&mut app, 120, 20), &["4 轮"]);
        // Moved up: no longer follows; another new one doesn't move it
        press(&mut app, KeyCode::Up);
        add(aca_records::trace_claude::fixtures::prompt("u5", Some("u4"), 70_000, "又一句"));
        app.trace_tail();
        trace_panes(&mut app, 120, 20);
        assert_ne!(trace_sel(&app), "又一句");
        // On a phone, following the latest also sticks to the bottom
        let (mut app, p) = trace_app("follows2");
        reader_rows(&mut app, 46, 10);
        writeln!(
            std::fs::OpenOptions::new().append(true).open(&p).unwrap(),
            "{}",
            aca_records::trace_claude::fixtures::prompt("u4", Some("a5"), 60_000, "下一句")
        )
        .unwrap();
        app.trace_tail();
        shows(&reader_rows(&mut app, 46, 10), &["你  下一句"]);
        assert_eq!(trace_sel(&app), "下一句");
    }

    #[test]
    fn shift_z_says_why_when_there_is_nothing_to_read() {
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('Z'));
        let _ = app.trace_request();
        app.set_trace("跑", Err("找不到记录".into()));
        shows(&reader_rows(&mut app, 60, 10), &["记录 · 跑", "找不到记录"]);
        // An old result coming back after the session changed is dropped
        press(&mut app, KeyCode::Char('J'));
        app.set_trace("跑", Err("旧的".into()));
        assert!(app.trace.as_ref().unwrap().err.is_none());
    }

    /// Look at it by hand: cargo test --release trace_look -- --ignored --nocapture
    #[test]
    #[ignore]
    fn trace_look() {
        let show = |app: &mut App, w: u16, h: u16, what: &str| {
            println!("[{what} · {w}x{h}]");
            println!("{}", reader_rows(app, w, h).join("\n"));
            println!("{}", "=".repeat(w as usize));
        };
        let (mut app, _) = trace_app("look");
        show(&mut app, 120, 26, "桌面 · 刚打开");
        for _ in 0..2 {
            press(&mut app, KeyCode::Char('p'));
        }
        show(&mut app, 120, 34, "桌面 · 第 1 轮");
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        show(&mut app, 120, 34, "桌面 · 进过程、选到 Edit");
        let (mut app, _) = trace_app("look2");
        show(&mut app, 46, 34, "手机 · 对话");
        for _ in 0..2 {
            press(&mut app, KeyCode::Char('p'));
        }
        press(&mut app, KeyCode::Enter);
        show(&mut app, 46, 40, "手机 · 第 1 轮的过程");
    }

    /// Check the colors by hand: ACA_TRACE_HTML=<dir> cargo test --release trace_html -- --ignored. Draws a few screens into a web page
    /// Draw one screen and turn it into colored HTML (for checking the layout yourself)
    fn html_page(app: &mut App, w: u16, h: u16) -> String {
        let css = |c: Color| match c {
            Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
            _ => None,
        };
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..h {
            let mut x = 0;
            while x < w {
                let c = &buf[(x, y)];
                let mut st = format!("display:inline-block;width:{}ch;", c.cell_width().max(1));
                if let Some(f) = css(c.fg) {
                    st += &format!("color:{f};");
                }
                if let Some(b) = css(c.bg) {
                    st += &format!("background:{b};");
                }
                if c.modifier.contains(Modifier::BOLD) {
                    st += "font-weight:bold;";
                }
                let sym = match c.symbol() {
                    " " => "&nbsp;".to_string(),
                    s => s.replace('&', "&amp;").replace('<', "&lt;"),
                };
                out += &format!("<span style=\"{st}\">{sym}</span>");
                x += c.cell_width().max(1);
            }
            out += "<br>";
        }
        format!(
            "<div style=\"display:inline-block;vertical-align:top;margin:12px;padding:6px;background:#eff1f5;color:#4c4f69;font:14px/1.25 Menlo,'PingFang SC',monospace;white-space:pre\">{out}</div>"
        )
    }

    #[test]
    #[ignore]
    fn trace_html() {
        let Ok(dir) = std::env::var("ACA_TRACE_HTML") else { return };
        let page = html_page;
        let (mut app, _) = trace_app("html");
        for _ in 0..2 {
            press(&mut app, KeyCode::Char('p'));
        }
        let mut html = page(&mut app, 120, 30);
        press(&mut app, KeyCode::Right);
        for _ in 0..4 {
            press(&mut app, KeyCode::Down);
        }
        html += &page(&mut app, 120, 30);
        // o unfolds Edit
        press(&mut app, KeyCode::Char('o'));
        html += &page(&mut app, 120, 30);
        // Unfold Read in the last turn (keys go by the last draw, so draw after each)
        for k in [KeyCode::Char('n'), KeyCode::Char('n'), KeyCode::Down, KeyCode::Down, KeyCode::Char('o')] {
            press(&mut app, k);
            page(&mut app, 120, 30);
        }
        html += &page(&mut app, 120, 30);
        // Just wide enough for two panes
        for k in [KeyCode::Char('p'), KeyCode::Char('p'), KeyCode::Char('O')] {
            press(&mut app, k);
            page(&mut app, 90, 40);
        }
        html += &page(&mut app, 90, 40);
        let (mut app, _) = trace_app("html2");
        html += &page(&mut app, 46, 30);
        for _ in 0..2 {
            press(&mut app, KeyCode::Char('p'));
        }
        press(&mut app, KeyCode::Enter);
        html += &page(&mut app, 46, 30);
        for k in [KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Char('o')] {
            press(&mut app, k);
            page(&mut app, 46, 30);
        }
        html += &page(&mut app, 46, 30);
        std::fs::write(format!("{dir}/trace.html"), format!("<html><body style=\"background:#dce0e8;margin:0\">{html}</body></html>"))
            .unwrap();
    }

    /// The subagent screens: ACA_TRACE_HTML=<dir> cargo test --release trace_html_subs -- --ignored
    #[test]
    #[ignore]
    fn trace_html_subs() {
        let Ok(dir) = std::env::var("ACA_TRACE_HTML") else { return };
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('Z'));
        let _ = app.trace_request();
        let (p, _, later) = aca_records::trace_claude::fixtures::with_subs("html-subs");
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        for l in &later {
            use std::io::Write;
            writeln!(f, "{l}").unwrap();
        }
        app.set_trace("跑", Ok(crate::trace::Trace::open_born(p, Agent::Claude, 0).unwrap()));
        let mut html = String::new();
        for k in [KeyCode::Char('p'), KeyCode::Right, KeyCode::Down, KeyCode::Down] {
            html_page(&mut app, 120, 30);
            press(&mut app, k);
        }
        html += &html_page(&mut app, 120, 30);
        press(&mut app, KeyCode::Char('o'));
        html += &html_page(&mut app, 120, 30);
        press(&mut app, KeyCode::Enter);
        html += &html_page(&mut app, 120, 30);
        html += &html_page(&mut app, 46, 30);
        std::fs::write(format!("{dir}/trace-subs.html"), format!("<html><body style=\"background:#dce0e8;margin:0\">{html}</body></html>"))
            .unwrap();
    }
}
