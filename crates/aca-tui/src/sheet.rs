//! The `?` sheet: the keys of the page it was pressed on, one per row, under headings. Pressing a row's key does it (the sheet
//! closes and the page gets that key, as if pressed there; a plugin's command runs); ⏎ does the highlighted one; `/` filters.
//! j k g G ↑ ↓ only move and Esc q ? only close: they never do a row (⏎ on that row still does).
//! On the main list it also has the legend on top and the plugins' commands first (they can only be pressed here); a key a
//! plugin binds that aca uses too goes to the plugin, and aca's row shows no key.
//! A page lists its keys once ([`Key`]); its bottom bar takes the ones marked for it, so the two call them the same.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use aca_core::plugin::{ItemKind, MenuItem};
use aca_core::{t, tr};

use crate::caps::{Form, Req, Target};
use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::theme;

/// One key on a page, as its bottom bar and `?` write it. `foot` = the bottom bar shows it too
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub key: String,
    pub what: String,
    pub foot: bool,
}

/// A key the bottom bar shows too
pub fn key(key: impl Into<String>, what: impl Into<String>) -> Key {
    Key { key: key.into(), what: what.into(), foot: true }
}

/// A key listed only in `?`
pub fn more(key: impl Into<String>, what: impl Into<String>) -> Key {
    Key { key: key.into(), what: what.into(), foot: false }
}

/// The bottom bar's part: (key, what) of the ones marked for it, in order
pub fn foot(keys: &[Key]) -> Vec<(&str, &str)> {
    keys.iter().filter(|k| k.foot).map(|k| (k.key.as_str(), k.what.as_str())).collect()
}

/// What a page calls one of its keys (for bottom bars laid out by hand); "" if it has none
pub fn named<'a>(keys: &'a [Key], label: &str) -> &'a str {
    keys.iter().find(|k| k.key == label).map_or("", |k| k.what.as_str())
}

/// The key a label stands for: one character, `^R`, ⏎, Tab, ⇧Tab, Esc, ⌫, arrows, ⇧↑ ⇧↓ ⇧⏎, 「空格」 (Space). Several keys in one
/// label (`↑↓`, `j k`, `1-9`) = None: that row only says, ⏎ on it does nothing
pub fn event(label: &str) -> Option<KeyEvent> {
    let plain = |c| Some(KeyEvent::from(c));
    let shift = |c| Some(KeyEvent::new(c, KeyModifiers::SHIFT));
    let mut cs = label.chars();
    match label {
        "⏎" => plain(KeyCode::Enter),
        "⇧⏎" => shift(KeyCode::Enter),
        "Esc" => plain(KeyCode::Esc),
        "Tab" => plain(KeyCode::Tab),
        "⇧Tab" => shift(KeyCode::BackTab),
        "⌫" => plain(KeyCode::Backspace),
        "↑" => plain(KeyCode::Up),
        "↓" => plain(KeyCode::Down),
        "←" => plain(KeyCode::Left),
        "→" => plain(KeyCode::Right),
        "⇧↑" => shift(KeyCode::Up),
        "⇧↓" => shift(KeyCode::Down),
        "空格" | "Space" | "space" => plain(KeyCode::Char(' ')),
        _ => match (cs.next(), cs.next(), cs.next()) {
            (Some('^'), Some(c), None) => Some(KeyEvent::new(KeyCode::Char(c.to_ascii_lowercase()), KeyModifiers::CONTROL)),
            (Some(c), None, _) => plain(KeyCode::Char(c)),
            _ => None,
        },
    }
}

/// Every key a label stands for: one key (see `event`), or several written with spaces (`J K`, `空格 b`, `^D ^U`) or run together
/// (`↑↓`, `jk`, `⇧↑↓`). A label with anything else in it (`1-9`, 「数字」) = none: that row only says
pub fn events(label: &str) -> Vec<KeyEvent> {
    if let Some(e) = event(label) {
        return vec![e];
    }
    let mut out: Vec<KeyEvent> = vec![];
    for t in label.split_whitespace() {
        let shifted = t.strip_prefix('⇧');
        let run = shifted.unwrap_or(t);
        let one = event(t).map(|e| vec![e]).or_else(|| {
            let ok = run.chars().count() > 1 && run.chars().all(|c| c.is_ascii_alphabetic() || "↑↓←→".contains(c));
            ok.then(|| run.chars().filter_map(|c| event(&c.to_string())).collect())
        });
        let Some(mut es) = one else { return vec![] };
        if shifted.is_some() {
            es.iter_mut().for_each(|e| e.modifiers |= KeyModifiers::SHIFT);
        }
        for e in es {
            if !out.contains(&e) {
                out.push(e);
            }
        }
    }
    out
}

/// Whether k is the key ev stands for. Ctrl must match; shift only for the keys that need it (⇧↑ ⇧↓ ⇧⏎), not a capital letter's own
pub fn same(ev: KeyEvent, k: KeyEvent) -> bool {
    let ctrl = |e: KeyEvent| e.modifiers.contains(KeyModifiers::CONTROL);
    let shift = |e: KeyEvent| e.modifiers.contains(KeyModifiers::SHIFT);
    let shifted = matches!(ev.code, KeyCode::Up | KeyCode::Down | KeyCode::Enter);
    ev.code == k.code && ctrl(ev) == ctrl(k) && (!shifted || shift(ev) == shift(k))
}

/// What a row does
#[derive(Clone, Debug, PartialEq)]
pub enum Do {
    /// Hand the page the key pressed (one of these; ⏎ on the row hands it the first when there's only one)
    Key(Vec<KeyEvent>),
    /// Run a plugin's command / view / pane on the target
    Plugin(Box<MenuItem>),
    /// Only says: several keys in one row, or keys pressed somewhere else (in tmux, while typing)
    Say,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// The key at the start; blank when a plugin took it (⏎ on the row still does aca's)
    pub key: String,
    pub what: String,
    /// Right side: which plugin it comes from (blank for aca's own)
    pub from: String,
    pub act: Do,
}

impl Row {
    pub fn new(key: &str, what: &str) -> Row {
        let evs = events(key);
        let act = if evs.is_empty() { Do::Say } else { Do::Key(evs) };
        Row { key: key.to_string(), what: what.to_string(), from: String::new(), act }
    }

    /// The key pressed is this row's
    fn hit(&self, k: KeyEvent) -> bool {
        match &self.act {
            _ if self.key.is_empty() => false,
            Do::Key(evs) => evs.iter().any(|e| same(*e, k)),
            Do::Plugin(it) => !k.modifiers.contains(KeyModifiers::CONTROL) && it.key.is_some_and(|c| k.code == KeyCode::Char(c)),
            Do::Say => false,
        }
    }

    fn matches(&self, f: &str) -> bool {
        [&self.key, &self.what, &self.from].iter().any(|s| s.to_lowercase().contains(f))
    }
}

enum Item {
    Head(String),
    Row(Row),
}

/// One column's width inside the frame (the sheet's own width when it fits in one)
const COL: usize = 66;
/// Side by side, a column is never narrower than this
const MIN_COL: usize = 48;
/// At most this many columns
const MAX_COLS: usize = 3;
/// Between two columns
const GAP: usize = 4;

/// The body laid out in columns (see `Sheet::columns`): each line marked whether it's a row
struct Columns {
    n: usize,
    cw: usize,
    cols: Vec<Vec<(bool, Line<'static>)>>,
    /// The column the highlight is in
    hl: usize,
}

/// Pour the body into columns room lines tall, top to bottom then the next: a heading doesn't end a column (it goes on to the next
/// with its rows), a column doesn't start with the blank line above a heading. Each line marked whether it's one of the rows; and
/// which column the line at is in
fn pour(body: Vec<Line<'static>>, at: usize, (rows, heads): (&[usize], &[usize]), room: usize) -> (Vec<Vec<(bool, Line<'static>)>>, usize) {
    let mut cols: Vec<Vec<(bool, Line<'static>)>> = vec![vec![]];
    let mut hl = 0;
    for (i, line) in body.into_iter().enumerate() {
        let len = cols.last().map_or(0, Vec::len);
        if len >= room || (heads.contains(&i) && len + 1 >= room && len > 0) {
            cols.push(vec![]);
        }
        if i == at {
            hl = cols.len() - 1;
        }
        let col = cols.last_mut().unwrap();
        if col.is_empty() && line.width() == 0 {
            continue;
        }
        col.push((rows.contains(&i), line));
    }
    (cols, hl)
}

pub struct Sheet {
    title: String,
    items: Vec<Item>,
    /// Draw the list's legend above the keys (the main list's sheet)
    legend: bool,
    /// What a plugin's row runs against
    target: Target,
    /// The highlighted row (index in `items`, always a row)
    sel: usize,
    /// The filter text being typed after `/`
    filter: Option<String>,
}

impl Sheet {
    /// A page's keys, under no heading
    pub fn page(keys: &[Key]) -> Sheet {
        let rows = keys.iter().map(|k| Row::new(&k.key, &k.what)).collect();
        Sheet::new(t!("快捷键", "Keys").into(), vec![(String::new(), rows)], false, Target::default())
    }

    /// Headings and their rows (a blank heading = rows without one); headings with no rows are left out
    pub fn new(title: String, secs: Vec<(String, Vec<Row>)>, legend: bool, target: Target) -> Sheet {
        let mut items = vec![];
        for (h, rows) in secs.into_iter().filter(|(_, r)| !r.is_empty()) {
            if !h.is_empty() {
                items.push(Item::Head(h));
            }
            items.extend(rows.into_iter().map(Item::Row));
        }
        let sel = items.iter().position(|i| matches!(i, Item::Row(_))).unwrap_or(0);
        Sheet { title, items, legend, target, sel, filter: None }
    }

    fn row(&self, i: usize) -> Option<&Row> {
        match self.items.get(i) {
            Some(Item::Row(r)) => Some(r),
            _ => None,
        }
    }

    /// The rows shown (indices), the filter applied
    fn rows(&self) -> Vec<usize> {
        let f = self.filter.as_deref().unwrap_or("").to_lowercase();
        (0..self.items.len()).filter(|&i| self.row(i).is_some_and(|r| f.is_empty() || r.matches(&f))).collect()
    }

    /// Move the highlight d rows through the shown ones (wrapping round)
    fn step(&mut self, d: isize) {
        let rows = self.rows();
        let n = rows.len() as isize;
        if n == 0 {
            return;
        }
        let at = rows.iter().position(|&i| i == self.sel).map_or(-1, |a| a as isize);
        self.sel = rows[((at + d).rem_euclid(n)) as usize];
    }

    /// Do row i: pressed = the key that picked it (None = ⏎ on the highlighted row)
    fn run(&self, i: usize, pressed: Option<KeyEvent>, fx: &mut Fx) -> Flow {
        let Some(r) = self.row(i) else { return Flow::Stay };
        match &r.act {
            Do::Key(evs) => match (pressed.and_then(|k| evs.iter().find(|e| same(**e, k))), evs.as_slice()) {
                (Some(e), _) | (None, [e]) => fx.key = Some(*e),
                // Several keys in one row: ⏎ can't tell which
                (None, _) => return Flow::Stay,
            },
            Do::Plugin(it) if it.kind == ItemKind::Command && !it.args.is_empty() => {
                fx.open = Some(Box::new(Form::new((**it).clone(), self.target.clone())));
            }
            Do::Plugin(it) => {
                crate::caps::start(Req { item: (**it).clone(), session: self.target.session.clone(), ..Default::default() }, fx)
            }
            Do::Say => return Flow::Stay,
        }
        Flow::Close
    }

    /// The lines between the title and the bottom: legend, then headings and rows; with the line the highlight is on, which lines
    /// are rows and which are headings
    fn body(&self, w: usize) -> (Vec<Line<'static>>, usize, Vec<usize>, Vec<usize>) {
        let shown = self.rows();
        let key_w = self.items.iter().filter_map(|i| if let Item::Row(r) = i { Some(r.key.width()) } else { None }).max().unwrap_or(0);
        let key_w = key_w.min(12);
        let mut out = vec![];
        let filtering = self.filter.as_ref().is_some_and(|f| !f.is_empty());
        if self.legend && !filtering {
            out.extend(crate::help::legend(w));
        }
        let mut at = 0;
        let (mut row_lines, mut head_lines) = (vec![], vec![]);
        let mut head: Option<&str> = None;
        for (i, it) in self.items.iter().enumerate() {
            match it {
                Item::Head(h) => head = Some(h),
                Item::Row(r) if shown.contains(&i) => {
                    // A heading goes above its first shown row
                    if let Some(h) = head.take() {
                        if !out.is_empty() {
                            out.push(Line::raw(""));
                        }
                        head_lines.push(out.len());
                        out.push(Line::styled(h.to_string(), Style::new().add_modifier(Modifier::BOLD)));
                    }
                    if i == self.sel {
                        at = out.len();
                    }
                    row_lines.push(out.len());
                    out.push(kit::picked(self.row_line(r, key_w, w), w, i == self.sel));
                }
                Item::Row(_) => {}
            }
        }
        if shown.is_empty() {
            out.push(Line::styled(t!("无匹配项", "nothing matches"), kit::dim()));
        }
        (out, at, row_lines, head_lines)
    }

    /// How many lines the body gets in a sheet h lines tall (inside its frame): less the title, its blank and the bottom
    fn room(&self, h: usize) -> usize {
        h.saturating_sub(2 + 1 + self.foot(0).len()).max(1)
    }

    /// Side by side, w wide and h tall: how many columns and how wide each, the columns' lines (the body poured top to bottom, one
    /// column after another) and which column the highlight is in. None = one column (it fits, or there's no room for two)
    fn columns(&self, w: usize, h: usize, room: usize) -> Option<Columns> {
        let most = ((w + GAP) / (MIN_COL + GAP)).min(MAX_COLS);
        if most < 2 || self.body(w).0.len() <= self.room(h) {
            return None;
        }
        for n in 2..=most {
            let cw = (w - GAP * (n - 1)) / n;
            let (body, at, rows, heads) = self.body(cw);
            let (cols, hl) = pour(body, at, (&rows, &heads), room);
            if cols.len() <= n || n == most {
                return Some(Columns { n, cw, cols, hl });
            }
        }
        None
    }

    /// The width inside the frame for a screen with avail columns to spare and h lines inside the frame: one column's when the
    /// body fits in one (or there's no room for two), else wide enough for as few columns as it fits in (up to three), else as wide
    /// as the screen allows
    pub fn inner_width(&self, avail: usize, h: usize) -> usize {
        let room = self.room(h);
        if avail < 2 * MIN_COL + GAP || self.body(COL).0.len() <= room {
            return COL;
        }
        let wide = |n: usize| (n * COL + (n - 1) * GAP).min(avail);
        for n in 2..=MAX_COLS {
            if wide(n) < n * MIN_COL + (n - 1) * GAP {
                break;
            }
            if self.columns(wide(n), h, room).is_some_and(|c| c.cols.len() <= c.n) {
                return wide(n);
            }
        }
        wide(MAX_COLS)
    }

    /// The body in columns side by side: the ones holding the highlight when they don't all fit (and how many rows are past
    /// them, for the line below)
    fn side_by_side(&self, c: Columns, room: usize) -> (Vec<Line<'static>>, usize) {
        let Columns { n, cw, cols, hl } = c;
        let from = (hl + 1).saturating_sub(n).min(cols.len().saturating_sub(n));
        let below = cols.iter().skip(from + n).flatten().filter(|(row, _)| *row).count();
        let shown = &cols[from..(from + n).min(cols.len())];
        let lines = (0..room)
            .map(|y| {
                let mut spans = vec![];
                for (i, col) in shown.iter().enumerate() {
                    let line = col.get(y).map(|(_, l)| l.clone()).unwrap_or_default();
                    let pad = if i + 1 < shown.len() { cw - line.width().min(cw) + GAP } else { 0 };
                    // The line's own style (the highlight's background) goes onto its parts, or it would be lost in the merge
                    let st = line.style;
                    spans.extend(line.spans.into_iter().map(|s| Span::styled(s.content, st.patch(s.style))));
                    spans.push(Span::raw(" ".repeat(pad)));
                }
                Line::from(spans)
            })
            .collect();
        (lines, below)
    }

    /// The title line; over = it doesn't all fit: the top right says which row the highlight is on, of how many
    fn title_line(&self, w: usize, over: bool) -> Line<'static> {
        let mut right = vec![];
        if over {
            let shown = self.rows();
            let pos = shown.iter().position(|&i| i == self.sel).map_or(0, |p| p + 1);
            right.push(Span::styled(format!("{pos} / {}  ", shown.len()), kit::dim()));
        }
        right.extend(kit::esc_hint(t!("关闭", "close")));
        let right_w: usize = right.iter().map(|s| s.content.width()).sum();
        let title = kit::clip(&self.title, w.saturating_sub(right_w + 2)).trim_end().to_string();
        kit::modal_title(&title, theme::accent(), right, w)
    }

    fn row_line(&self, r: &Row, key_w: usize, w: usize) -> Line<'static> {
        let key = kit::clip(&r.key, key_w);
        let key = format!(" {key}{}  ", " ".repeat(key_w.saturating_sub(key.width())));
        let right = r.from.clone();
        let room = w.saturating_sub(key.width() + right.width() + 2);
        let what = kit::clip(&r.what, room).trim_end().to_string();
        let st = if r.act == Do::Say { kit::dim() } else { Style::new() };
        kit::spread(vec![Span::styled(key, kit::key_style()), Span::styled(what, st)], vec![Span::styled(right, kit::dim())], w)
    }

    fn foot(&self, w: usize) -> Vec<Line<'static>> {
        match &self.filter {
            Some(f) => vec![
                Line::from(vec![Span::styled(t!("筛选 > ", "filter > "), theme::accent()), Span::raw(f.clone()), Span::raw("▏")]),
                kit::keys_line(&[("⏎", t!("执行", "run")), ("Esc", t!("清除筛选", "stop filtering"))], vec![], w),
            ],
            None => vec![kit::keys_line(
                &[(t!("按键", "a key"), t!("执行", "runs it")), ("⏎", t!("执行高亮", "run this")), ("/", t!("筛选", "filter"))],
                vec![],
                w,
            )],
        }
    }
}

impl Overlay for Sheet {
    fn width(&self) -> u16 {
        72
    }

    fn fill(&self) -> bool {
        true
    }

    /// As tall as everything in it (title, legend, rows, bottom), so it doesn't change size as the highlight moves or a filter is typed
    fn fill_rows(&self) -> Option<u16> {
        let heads = self.items.iter().filter(|i| matches!(i, Item::Head(_))).count();
        let legend = if self.legend { crate::help::legend(66).len() } else { 0 };
        Some((2 + legend + self.items.len() + heads + 3) as u16)
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        self.lines_in(w, 40)
    }

    fn lines_in(&self, w: usize, h: usize) -> Vec<Line<'static>> {
        let mut foot = vec![Line::raw("")];
        foot.extend(self.foot(w));
        let room = h.saturating_sub(2 + foot.len()).max(1);
        // Too long for one column and wide enough: columns side by side; only when even those don't hold it all does it say where
        // the highlight is and how many are left
        let fits = self.columns(w, h, room).filter(|c| c.cols.len() <= c.n);
        if let Some(c) = fits.or_else(|| self.columns(w, h, room.saturating_sub(1))) {
            let over = c.cols.len() > c.n;
            let (body, below) = self.side_by_side(c, if over { room.saturating_sub(1) } else { room });
            let mut out = vec![self.title_line(w, over), Line::raw("")];
            out.extend(body);
            if below > 0 {
                out.push(Line::styled(tr!(" ↓ 还有 {below} 个", " ↓ {below} more"), kit::dim()));
            }
            out.resize(h.saturating_sub(foot.len()), Line::raw(""));
            out.extend(foot);
            return out;
        }
        let (body, at, row_lines, _) = self.body(w);
        // Keep the highlight a few rows from the edge; the top stays put while it's near the top (the legend shows)
        let place = |room: usize| at.saturating_sub(room.saturating_sub(3)).min(body.len().saturating_sub(room));
        let over = body.len() > room;
        let (mut from, mut take) = (place(room), room);
        // More below: the last line says how many rows
        if over && from + take < body.len() {
            take = room.saturating_sub(1).max(1);
            from = place(take);
            // Not ending on a heading (or the blank above it) whose rows are all below
            while take > 1 && from + take - 1 > at && !row_lines.contains(&(from + take - 1)) {
                take -= 1;
            }
        }
        let below = row_lines.iter().filter(|&&l| l >= from + take).count();
        let mut out = vec![self.title_line(w, over), Line::raw("")];
        out.extend(body.into_iter().skip(from).take(take));
        if below > 0 {
            out.push(Line::styled(tr!(" ↓ 还有 {below} 个", " ↓ {below} more"), kit::dim()));
        }
        out.resize(h.saturating_sub(foot.len()), Line::raw(""));
        out.extend(foot);
        out
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(f) = &mut self.filter {
            match k.code {
                KeyCode::Esc => self.filter = None,
                KeyCode::Enter => return self.run(self.sel, None, fx),
                KeyCode::Up => self.step(-1),
                KeyCode::Down => self.step(1),
                KeyCode::Backspace => drop(f.pop()),
                KeyCode::Char(c) if !ctrl => f.push(c),
                _ => {}
            }
            // After filtering, the highlight lands on the first match
            let rows = self.rows();
            if !rows.contains(&self.sel) {
                self.sel = rows.first().copied().unwrap_or(self.sel);
            }
            return Flow::Stay;
        }
        match k.code {
            KeyCode::Esc => return Flow::Close,
            KeyCode::Char('q' | '?') if !ctrl => return Flow::Close,
            KeyCode::Enter => return self.run(self.sel, None, fx),
            KeyCode::Up => self.step(-1),
            KeyCode::Down => self.step(1),
            KeyCode::PageUp => self.step(-10),
            KeyCode::PageDown => self.step(10),
            KeyCode::Char('k') if !ctrl => self.step(-1),
            KeyCode::Char('j') if !ctrl => self.step(1),
            KeyCode::Home => self.sel = self.rows().first().copied().unwrap_or(self.sel),
            KeyCode::End => self.sel = self.rows().last().copied().unwrap_or(self.sel),
            KeyCode::Char('g') if !ctrl => self.sel = self.rows().first().copied().unwrap_or(self.sel),
            KeyCode::Char('G') if !ctrl => self.sel = self.rows().last().copied().unwrap_or(self.sel),
            KeyCode::Char('/') if !ctrl => self.filter = Some(String::new()),
            _ => {
                if let Some(i) = (0..self.items.len()).find(|&i| self.row(i).is_some_and(|r| r.hit(k))) {
                    return self.run(i, Some(k), fx);
                }
                // A key no row stands for (a workspace's letter in S's, a number on the list's): as if pressed on the page below
                fx.key = Some(k);
                return Flow::Close;
            }
        }
        Flow::Stay
    }

    fn typing(&self) -> bool {
        self.filter.is_some()
    }

    fn sheet(&self) -> Option<Vec<Key>> {
        None
    }
}

impl Sheet {
    /// What aca's own row of this key is called here (the same key means something else on this row); a plugin's isn't touched
    pub fn rename(&mut self, key: &str, what: &str) {
        for it in &mut self.items {
            if let Item::Row(r) = it
                && r.key == key
                && r.from.is_empty()
                && r.act != Do::Say
            {
                r.what = what.to_string();
            }
        }
    }
}

/// The main list's sheet: the plugins' commands that fit the highlighted row first, then aca's own by section (acts.rs), the
/// legend on top. `desk`: the row is the rail's row for all groups, where the keys that reach the desk work too
pub fn main(all: Vec<MenuItem>, target: Target, desk: bool) -> Sheet {
    let plugins: Vec<Row> = all
        .into_iter()
        .filter(|i| i.fits(target.on))
        .map(|it| Row {
            key: it.key.map_or(String::new(), |k| k.to_string()),
            what: format!("{}{}", it.title, if it.args.is_empty() { "" } else { "…" }),
            from: it.plugin_name.clone(),
            act: Do::Plugin(Box::new(it)),
        })
        .collect();
    let bound: Vec<String> = plugins.iter().map(|r| r.key.clone()).filter(|k| !k.is_empty()).collect();
    let mut secs = vec![(t!("插件", "Plugins").to_string(), plugins)];
    for (i, name) in crate::acts::sections().into_iter().enumerate() {
        let rows = crate::acts::ACTS
            .iter()
            .filter(|a| a.sec == i && (a.fits(target.on) || desk && a.fits("desk")))
            .map(|a| {
                let mut r = Row::new(a.label(), a.name());
                // Pressed in tmux or while making a new session: only listed here
                if crate::acts::elsewhere(a) {
                    r.act = Do::Say;
                }
                // A plugin took this key: it goes to the plugin, aca's row shows none (⏎ still does aca's)
                if bound.contains(&r.key) && r.act != Do::Say {
                    r.key.clear();
                }
                r
            })
            .collect();
        secs.push((name.to_string(), rows));
    }
    let title = match target.on {
        "session" => tr!("快捷键 · 对「{}」", "Keys · for “{}”", target.label),
        "group" => tr!("快捷键 · 对分组 {}", "Keys · for group {}", target.label),
        _ => t!("快捷键", "Keys").to_string(),
    };
    Sheet::new(title, secs, true, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aca_core::i18n::{self, Lang};

    fn press(s: &mut Sheet, k: KeyEvent) -> (Flow, Fx) {
        let mut fx = Fx::default();
        let f = s.on_key(k, &mut fx);
        (f, fx)
    }

    fn ch(c: char) -> KeyEvent {
        KeyEvent::from(KeyCode::Char(c))
    }

    fn text(s: &Sheet, w: usize, h: usize) -> Vec<String> {
        s.lines_in(w, h).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    #[test]
    fn labels_turn_into_the_keys_they_stand_for() {
        assert_eq!(event("x"), Some(ch('x')));
        assert_eq!(event("^R"), Some(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)));
        assert_eq!(event("空格"), Some(ch(' ')));
        assert_eq!(event("⇧↑"), Some(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT)));
        assert_eq!(event("⏎"), Some(KeyEvent::from(KeyCode::Enter)));
        for several in ["↑↓", "j k", "1-9", "r R", "数字"] {
            assert_eq!(event(several), None, "{several}");
        }
        // Several in one label: each of them; something that isn't a key makes it a row that only says
        let up = |m| KeyEvent::new(KeyCode::Up, m);
        assert_eq!(events("J K"), [ch('J'), ch('K')]);
        assert_eq!(events("空格 b"), [ch(' '), ch('b')]);
        assert_eq!(events("↑↓ jk"), [up(KeyModifiers::NONE), KeyEvent::from(KeyCode::Down), ch('j'), ch('k')]);
        assert_eq!(events("⇧↑↓"), [up(KeyModifiers::SHIFT), KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT)]);
        assert_eq!(events("q q"), [ch('q')]);
        assert!(events("1-9").is_empty() && events("数字").is_empty() && events("前缀 a").is_empty());
        // A capital letter arrives with shift: still the same key; ⇧↑ needs its shift, ^R its ctrl
        assert!(same(ch('F'), KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT)));
        assert!(!same(event("⇧↑").unwrap(), KeyEvent::from(KeyCode::Up)));
        assert!(!same(event("^R").unwrap(), ch('r')));
    }

    #[test]
    fn a_rows_key_does_it_enter_does_the_highlighted_one_and_moving_keys_never_do_a_row() {
        i18n::with(Lang::Zh, || {
            let keys =
                [key("^R", "刷新"), key("i", "登录"), more("j", "一个占了 j 的键"), key("1-9", "选这一个"), key("n p", "下一个 / 上一个")];
            let mut s = Sheet::page(&keys);
            let t = text(&s, 60, 12);
            assert!(t[0].contains("快捷键") && t.iter().any(|l| l.trim_start().starts_with("^R") && l.contains("刷新")), "{t:#?}");
            // ^R: closes and hands the page ^R
            let (f, fx) = press(&mut s, KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
            assert_eq!((f, fx.key), (Flow::Close, event("^R")));
            // j only moves (onto i), then onto the j row; ⏎ does that row
            assert_eq!(press(&mut s, ch('j')).1.key, None);
            assert_eq!(s.row(s.sel).unwrap().key, "i");
            press(&mut s, ch('j'));
            let (f, fx) = press(&mut s, KeyEvent::from(KeyCode::Enter));
            assert_eq!((f, fx.key), (Flow::Close, Some(ch('j'))));
            // A row that only says, or one with several keys: ⏎ does nothing; pressing one of its keys does that one
            press(&mut s, ch('j'));
            assert_eq!(press(&mut s, KeyEvent::from(KeyCode::Enter)).0, Flow::Stay);
            press(&mut s, ch('G'));
            assert_eq!(press(&mut s, KeyEvent::from(KeyCode::Enter)).0, Flow::Stay);
            let (f, fx) = press(&mut s, ch('p'));
            assert_eq!((f, fx.key), (Flow::Close, Some(ch('p'))));
            let mut s = Sheet::page(&keys);
            press(&mut s, ch('g'));
            assert_eq!(s.row(s.sel).unwrap().key, "^R");
            for k in [ch('q'), ch('?'), KeyEvent::from(KeyCode::Esc)] {
                let (f, fx) = press(&mut s, k);
                assert_eq!((f, fx.key), (Flow::Close, None));
            }
            // A key no row has (or one a row only names, like 1-9): closes and hands the page that key
            for k in [ch('z'), ch('3')] {
                let mut s = Sheet::page(&keys);
                let (f, fx) = press(&mut s, k);
                assert_eq!((f, fx.key), (Flow::Close, Some(k)));
            }
        });
    }

    #[test]
    fn when_it_doesnt_all_fit_the_title_says_which_row_and_the_bottom_how_many_are_left() {
        i18n::with(Lang::Zh, || {
            let keys: Vec<Key> = (0..20).map(|i| key(((b'a' + i) as char).to_string(), format!("第 {i} 个"))).collect();
            let mut s = Sheet::page(&keys);
            // 12 lines: title, blank, 8 for the body, blank, bottom keys: 7 rows show, the last line counts the other 13
            let t = text(&s, 60, 12);
            assert!(t[0].contains("快捷键") && t[0].contains("1 / 20") && t[0].contains("Esc 关闭"), "{t:#?}");
            assert_eq!(t[9].trim(), "↓ 还有 13 个", "{t:#?}");
            // Further down: the count follows the highlight; at the bottom nothing is left below
            for _ in 0..9 {
                press(&mut s, KeyEvent::from(KeyCode::Down));
            }
            let t = text(&s, 60, 12);
            assert!(t[0].contains("10 / 20") && t.iter().any(|l| l.contains("第 9 个")) && t[9].contains("还有"), "{t:#?}");
            press(&mut s, ch('G'));
            let t = text(&s, 60, 12);
            assert!(t[0].contains("20 / 20") && !t.iter().any(|l| l.contains("还有")) && t[9].contains("第 19 个"), "{t:#?}");
            // All of it fits: no count
            let t = text(&s, 60, 30);
            assert!(!t[0].contains(" / ") && !t.iter().any(|l| l.contains("还有")), "{t:#?}");
        });
    }

    /// Three sections of 10 keys each: 35 lines in one column
    fn long_sheet() -> Sheet {
        let letters: Vec<char> = ('a'..='z').chain('A'..='D').collect();
        let rows = |sec: usize| (0..10).map(|i| Row::new(&letters[sec * 10 + i].to_string(), &format!("第 {sec}-{i} 个"))).collect();
        Sheet::new("快捷键".into(), (0..3).map(|sec| (format!("节 {sec}"), rows(sec))).collect(), false, Target::default())
    }

    #[test]
    fn too_long_for_one_column_on_a_wide_screen_it_goes_into_columns_read_down_then_across() {
        i18n::with(Lang::Zh, || {
            let mut s = long_sheet();
            let col = |t: &[String], want: &str| {
                let (y, l) = t.iter().enumerate().find(|(_, l)| l.contains(want)).unwrap_or_else(|| panic!("{want}: {t:#?}"));
                (y, l[..l.find(want).unwrap()].width())
            };
            // 24 tall leaves 20 lines: two columns hold it all, no count anywhere
            let t = text(&s, 140, 24);
            assert!(!t[0].contains(" / ") && !t.iter().any(|l| l.contains("还有")), "{t:#?}");
            let (top, left) = col(&t, "节 0");
            assert_eq!(top, 2);
            // Sections run down the first column and on into the next
            assert_eq!((col(&t, "节 1").1, col(&t, "第 1-6 个")), (left, (top + 19, left + 4)), "{t:#?}");
            let (y, x) = col(&t, "第 1-7 个");
            assert!(y == top && x > left + 50, "{t:#?}");
            // The sheet widens for it: two columns of the usual width on a wide screen; one when it fits in one or there's no room
            assert_eq!(s.inner_width(190, 24), 2 * COL + GAP);
            assert_eq!((s.inner_width(190, 60), s.inner_width(80, 24)), (COL, COL));
            // j goes in reading order: from the first column's last row to the top of the next, highlighted there
            s.sel = (0..s.items.len()).find(|&i| s.row(i).is_some_and(|r| r.what == "第 1-6 个")).unwrap();
            press(&mut s, ch('j'));
            assert_eq!(s.row(s.sel).unwrap().what, "第 1-7 个");
            let lit =
                s.lines_in(140, 24).iter().position(|l| l.spans.iter().any(|x| x.content.contains("第 1-7 个") && x.style.bg.is_some()));
            assert_eq!(lit, Some(top), "{t:#?}");
            // 13 lines a column: three columns; a heading never sits on a column's last line, a column doesn't start with the blank
            // above one
            let t = text(&s, 170, 17);
            assert!(
                !t.iter().any(|l| l.contains("还有")) && t[top].contains("节 0") && t[top].contains("节 1") && t[top].contains("节 2"),
                "{t:#?}"
            );
            assert_eq!((s.inner_width(220, 17), s.inner_width(190, 17)), (3 * COL + 2 * GAP, 190));
            // Too narrow for three: two, and it says how many are left and where the highlight is; G keeps the highlight in view
            let t = text(&s, 110, 12);
            assert!(t[0].contains(" / 30") && t.iter().any(|l| l.contains("还有")), "{t:#?}");
            press(&mut s, ch('G'));
            let t = text(&s, 110, 12);
            assert!(
                t[0].contains("30 / 30") && t.iter().any(|l| l.contains("第 2-9 个")) && !t.iter().any(|l| l.contains("还有")),
                "{t:#?}"
            );
            press(&mut s, ch('g'));
            assert!(text(&s, 110, 12).iter().any(|l| l.contains("第 0-0 个")));
            // A phone: one column as before
            let t = text(&s, 42, 24);
            assert!(t.iter().all(|l| l.width() <= 42) && t[0].contains("1 / 30") && col(&t, "第 0-0 个").1 == 4, "{t:#?}");
            // Filtered down to what fits in one: back to one column
            press(&mut s, ch('/'));
            for c in "第 1-".chars() {
                press(&mut s, ch(c));
            }
            let t = text(&s, 140, 24);
            assert!(!t.iter().any(|l| l.contains("第 0-") || l.contains("第 2-")), "{t:#?}");
            assert_eq!(col(&t, "第 1-9 个").1, col(&t, "第 1-0 个").1);
            assert_eq!(s.inner_width(190, 24), COL);
        });
    }

    #[test]
    fn the_list_sheet_is_sectioned_by_what_you_list_the_sessions_and_the_pages() {
        i18n::with(Lang::Zh, || {
            let s = main(vec![], Target { on: "session", session: "id".into(), label: "x".into() }, false);
            let t = text(&s, 72, 200);
            let at = |want: &str| t.iter().position(|l| l.trim() == want).unwrap_or_else(|| panic!("{want}: {t:#?}"));
            let row = |k: &str| t.iter().position(|l| l.trim_start().starts_with(&format!("{k} "))).unwrap();
            let (list, sessions, pages, tmux) = (at("列出哪些"), at("会话"), at("页面"), at("tmux 中"));
            assert!(!t.iter().any(|l| ["筛选", "视图"].contains(&l.trim())), "{t:#?}");
            for k in ["/", "w", "0", "F", "W", "S"] {
                assert!((list..sessions).contains(&row(k)), "{k}: {t:#?}");
            }
            for k in ["$", "P", "^S", "L", "^E"] {
                assert!((pages..tmux).contains(&row(k)), "{k}: {t:#?}");
            }
            // a is a way to move now (to all groups, with the jump letters): the accounts page is reached from ^S
            assert!(row("a") < list, "{t:#?}");
            assert!((sessions..pages).contains(&row("Q")), "{t:#?}");
        });
    }

    #[test]
    fn slash_filters_and_enter_does_the_first_match() {
        i18n::with(Lang::Zh, || {
            let mut s = Sheet::page(&[key("i", "登录"), key("+", "添加账号"), key("d", "删除")]);
            press(&mut s, ch('/'));
            assert!(s.typing());
            for c in "删".chars() {
                press(&mut s, ch(c));
            }
            let t = text(&s, 60, 12);
            assert!(t.iter().any(|l| l.contains("d  删除")) && !t.iter().any(|l| l.contains("登录")), "{t:#?}");
            // While filtering, q is typed, not closing
            assert_eq!(press(&mut s, ch('q')).0, Flow::Stay);
            press(&mut s, KeyEvent::from(KeyCode::Backspace));
            let (f, fx) = press(&mut s, KeyEvent::from(KeyCode::Enter));
            assert_eq!((f, fx.key), (Flow::Close, Some(ch('d'))));
        });
    }

    #[test]
    fn the_main_sheet_puts_plugins_first_and_a_key_a_plugin_took_goes_to_it() {
        i18n::with(Lang::Zh, || {
            let ci = MenuItem {
                plugin: "p.x".into(),
                plugin_name: "PX".into(),
                id: "ci".into(),
                kind: ItemKind::View,
                title: "看 CI".into(),
                key: Some('c'),
                ..Default::default()
            };
            let target = Target { on: "session", session: "s1".into(), label: "跑".into() };
            let mut s = main(vec![ci], target, false);
            let t = text(&s, 70, 200);
            assert!(t[0].contains("快捷键 · 对「跑」"), "{t:#?}");
            let at = |w: &str| t.iter().position(|l| l.contains(w)).unwrap_or_else(|| panic!("{w}: {t:#?}"));
            // Legend on top, then the plugins, then aca's by section
            assert!(at("图例") < at("插件") && at("插件") < at("看 CI") && at("看 CI") < at("导航"), "{t:#?}");
            assert!(t[at("看 CI")].ends_with("PX"), "{}", t[at("看 CI")]);
            // c goes to the plugin; aca's copy row has no key
            let (f, fx) = press(&mut s, ch('c'));
            assert_eq!((f, fx.plugin.map(|r| r.item.id)), (Flow::Close, Some("ci".to_string())));
            assert!(t.iter().any(|l| l.trim_start().starts_with("复制")), "{t:#?}");
            // aca's: the key goes to the list
            let (f, fx) = press(&mut s, ch('f'));
            assert_eq!((f, fx.key), (Flow::Close, Some(ch('f'))));
        });
    }
}
