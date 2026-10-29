//! Trash: sessions deleted with `d`, and deleted groups (with their subgroups and the sessions in them), stay here for 1 day;
//! within that, `⏎` on the `b` page brings them back, and a fresh deletion can also be undone with `^Z` (see undo.rs); only
//! after 1 day are they deleted for good. One deletion is one entry: a group deleted with its sessions is also one entry, and
//! they come back together.
//!
//! Delete = archive (the program and tmux session are stopped, the registration and conversation kept); a group's registration
//! is deleted and recorded here as it was;
//! restore = recreate the groups as recorded and unarchive (see `Job::Restore` in ops.rs).
//! Deleting for good is done in passing by the interface as it opens, while it's open (a look on every refresh) and as it
//! exits (aca has no resident process): it can only be late, never early.
//! Only what's still archived is deleted for good: a session unarchived with `aca unarchive` in the meantime is wanted again,
//! so it stays.
//!
//! Stored in ~/.local/state/aca/ui/deleted, one entry per line (JSON). Like pins, it's reread on every refresh and before every
//! change, so an interface open on the phone and one on the desktop agree.
//! Older versions kept it in trash in the same directory (`<pid of the interface that deleted it>\t<profile>\t<session id>`,
//! deleted for good when the interface exited): entries whose interface has exited are moved over and kept for 1 day from the
//! move; those of an old interface still open are left to it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde::{Deserialize, Serialize};

use aca_core::store::Store;
use aca_core::{t, tr};

use crate::ops::Job;
use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::state::{state_file, write_atomic};
use crate::theme;

/// How long deleted ones are kept (seconds)
pub const KEEP: u64 = 24 * 3600;

/// At most this many entries on the page; beyond that it scrolls with the highlight
const SHOWN: usize = 12;

/// One deletion
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// This entry's key
    pub id: String,
    /// When it was deleted (seconds)
    pub at: u64,
    pub profile: String,
    /// Which group was deleted; empty = a session was deleted
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    /// Groups deleted with it (itself and its subgroups), recreated as they were
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<GroupRec>,
    /// Sessions deleted with it
    #[serde(default)]
    pub sessions: Vec<Gone>,
}

/// A deleted group as it was in the registry
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupRec {
    pub path: String,
    /// Order in the registry
    pub order: i64,
    /// Default directory
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dir: String,
    /// Which workspace it's in (the interface's, see spaces.rs)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<char>,
    /// Its note (the interface's, see notes.rs)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// One deleted session
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gone {
    pub id: String,
    pub title: String,
    /// Which group it was in (empty = none)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    /// Already archived before the delete: stays archived when restored
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
}

impl Item {
    /// The entry for deleting one session
    pub fn session(profile: &str, s: Gone) -> Item {
        Item::sessions(profile, vec![s])
    }

    /// Several sessions deleted together with v select: one entry, restored together
    pub fn sessions(profile: &str, v: Vec<Gone>) -> Item {
        Item { id: new_id(), at: aca_core::paths::now(), profile: profile.into(), group: String::new(), groups: vec![], sessions: v }
    }

    /// What the trash page and messages call it: a group by its name, a session by its title
    pub fn label(&self) -> &str {
        match self.sessions.first() {
            Some(s) if self.group.is_empty() => &s.title,
            _ => &self.group,
        }
    }

    /// What messages call it: a session is "<title>" (long ones cut), several deleted together are "<first>" and n-1 more, a
    /// group is "group <name>". No leading space (as in 「已恢复{}」 (restored …))
    pub fn name(&self) -> String {
        if self.group.is_empty() {
            crate::ops::named(self.label(), self.sessions.len())
        } else {
            let short: String = self.group.chars().take(16).collect();
            let more = if self.group.chars().count() > 16 { "…" } else { "" };
            tr!("分组 {short}{more}", "group {short}{more}")
        }
    }

    /// How many get unarchived on restore (ones already archived before the delete don't count)
    pub fn taken(&self) -> usize {
        self.sessions.iter().filter(|s| !s.archived).count()
    }

    fn expired(&self, now: u64) -> bool {
        self.at + KEEP <= now
    }
}

/// An entry's key: the time + a few random characters
pub fn new_id() -> String {
    format!("{}-{}", aca_core::paths::now_ms(), aca_core::paths::short_id())
}

#[derive(Clone, Default)]
pub struct Trash {
    /// None = in memory only (unit tests, --snapshot)
    path: Option<PathBuf>,
    items: Vec<Item>,
}

impl Trash {
    pub fn default_path() -> Option<PathBuf> {
        state_file("deleted")
    }

    pub fn open(path: Option<PathBuf>) -> Self {
        let mut t = Trash { path, items: vec![] };
        t.reload();
        t
    }

    /// Cut loose from the file once read; later changes stay in memory only
    pub fn detached(mut self) -> Self {
        self.path = None;
        self
    }

    pub fn reload(&mut self) {
        let Some(p) = &self.path else { return };
        self.items = std::fs::read_to_string(p).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(p) = &self.path else { return Ok(()) };
        let text: String = self.items.iter().filter_map(|i| serde_json::to_string(i).ok()).map(|l| l + "\n").collect();
        write_atomic(p, &text)
    }

    /// This session is in the trash
    pub fn has(&self, profile: &str, id: &str) -> bool {
        self.items.iter().any(|i| i.profile == profile && i.sessions.iter().any(|s| s.id == id))
    }

    /// Ids of all sessions in the trash
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.items.iter().flat_map(|i| i.sessions.iter().map(|s| s.id.as_str()))
    }

    pub fn get(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Ones not expired yet, most recently deleted first
    pub fn items(&self, now: u64) -> Vec<Item> {
        let mut v: Vec<Item> = self.items.iter().filter(|i| !i.expired(now)).cloned().collect();
        v.sort_by_key(|i| std::cmp::Reverse(i.at));
        v
    }

    /// Some have expired and are due to be deleted for good
    pub fn due(&self, now: u64) -> bool {
        self.items.iter().any(|i| i.expired(now))
    }

    pub fn add(&mut self, item: Item) -> std::io::Result<()> {
        self.reload();
        self.items.retain(|i| i.id != item.id);
        self.items.push(item);
        self.save()
    }

    /// Restored / deleted for good
    pub fn forget(&mut self, id: &str) -> std::io::Result<()> {
        self.reload();
        self.items.retain(|i| i.id != id);
        self.save()
    }

    /// Take the expired ones out of the file
    fn take_due(&mut self, now: u64) -> std::io::Result<Vec<Item>> {
        self.reload();
        let (due, keep): (Vec<Item>, Vec<Item>) = std::mem::take(&mut self.items).into_iter().partition(|i| i.expired(now));
        self.items = keep;
        if !due.is_empty() {
            self.save()?;
        }
        Ok(due)
    }
}

/// The process with that pid is still around (tries sending signal 0)
fn alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Delete for good, right now, the sessions in this entry that are still archived. Returns how many were deleted
pub fn drop_now(store: &Store, item: &Item) -> usize {
    let still = |id: &str| store.get(id).is_ok_and(|s| s.id == id && s.archived > 0);
    item.sessions.iter().filter(|s| still(&s.id) && aca_core::session::remove(store, &s.id).is_ok()).count()
}

/// Delete expired ones for good (first moving over what the old version recorded)
pub fn purge() {
    let Ok(store) = Store::open() else { return };
    let mut bin = Trash::open(Trash::default_path());
    if let Some(old) = state_file("trash") {
        let _ = legacy(&old, &mut bin, &store, alive);
    }
    let Ok(due) = bin.take_due(aca_core::paths::now()) else { return };
    for i in due {
        drop_now(&store, &i);
    }
}

/// The old version's trash: entries whose interface has exited (pid 0 / process gone) and that are still archived move into
/// the trash, kept for 1 day from now
fn legacy(old: &Path, bin: &mut Trash, store: &Store, alive: impl Fn(u32) -> bool) -> std::io::Result<()> {
    let Ok(text) = std::fs::read_to_string(old) else { return Ok(()) };
    let mut keep = String::new();
    let mut moved = vec![];
    for l in text.lines() {
        let mut f = l.split('\t');
        let (Some(pid), Some(profile), Some(id)) = (f.next().and_then(|p| p.parse::<u32>().ok()), f.next(), f.next()) else { continue };
        if pid != 0 && alive(pid) {
            keep.push_str(l);
            keep.push('\n');
        } else if let Ok(s) = store.get(id)
            && s.id == id
            && s.archived > 0
        {
            let title = aca_core::status::title(&s, &aca_core::runtime::load(&s.id));
            moved.push(Item::session(profile, Gone { id: s.id, title, group: s.group, archived: false }));
        }
    }
    for i in moved {
        bin.add(i)?;
    }
    if keep != text {
        write_atomic(old, &keep)?;
    }
    Ok(())
}

/// When the interface exits: start an `aca ui --purge` to delete expired ones for good. In its own process group and detached
/// from the terminal, so it finishes even if the popup closes / the terminal drops
pub fn purge_detached() {
    let Some(me) = aca_core::agent::self_path() else { return };
    let _ =
        aca_core::proc::detach(Command::new(me).args(["ui", "--purge"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()));
}

/// `b`: the trash. One deleted entry per row, newest on top; `⏎` restores (sessions come back stopped), `d` then `y` deletes for
/// good now.
/// `v` select as in the main list (as in yazi): `v` range-selects, `V` range-unchecks, Space checks one; `⏎` / `d` act on all
/// the selected ones together
pub struct Page {
    trash: Trash,
    now: fn() -> u64,
    rows: Vec<Item>,
    sel: usize,
    /// d pressed, waiting for y
    armed: bool,
    /// Ones deleted for good from this page
    dropped: Vec<String>,
    /// The checked ones (entry ids)
    marked: Vec<String>,
    /// v / V range: from this entry (id) to the highlighted row; Esc fixes it
    anchor: Option<String>,
    /// This range is a V range that unchecks
    unpick: bool,
}

impl Page {
    pub fn new(trash: Trash, now: fn() -> u64) -> Page {
        let mut p = Page { trash, now, rows: vec![], sel: 0, armed: false, dropped: vec![], marked: vec![], anchor: None, unpick: false };
        p.reload();
        p
    }

    fn cur(&self) -> Option<&Item> {
        self.rows.get(self.sel)
    }

    /// The range in progress: from the starting entry to the highlighted one
    fn range(&self) -> Option<std::ops::RangeInclusive<usize>> {
        let a = self.rows.iter().position(|r| self.anchor.as_ref() == Some(&r.id))?;
        Some(a.min(self.sel)..=a.max(self.sel))
    }

    /// The selected ones: the checked ones plus the range in progress (minus it, for an unchecking range), in page order
    fn chosen(&self) -> Vec<&Item> {
        let range = self.range();
        let ranged = |i: usize| range.as_ref().is_some_and(|r| r.contains(&i));
        let on = |i: usize, r: &Item| match self.unpick {
            true => self.marked.contains(&r.id) && !ranged(i),
            false => self.marked.contains(&r.id) || ranged(i),
        };
        self.rows.iter().enumerate().filter(|&(i, r)| on(i, r)).map(|(_, r)| r).collect()
    }

    /// What ⏎ / d act on: the selected ones if any, otherwise the highlighted entry
    fn targets(&self) -> Vec<Item> {
        match self.chosen() {
            c if c.is_empty() => self.cur().cloned().into_iter().collect(),
            c => c.into_iter().cloned().collect(),
        }
    }

    /// Whether the highlighted entry is checked: whether Space checks or unchecks it
    fn on_marked(&self) -> bool {
        self.cur().is_some_and(|r| self.marked.contains(&r.id))
    }

    /// v range-selects from the highlighted entry, V (unpick) range-unchecks; pressed again mid-range: the unfixed range is
    /// dropped and a new one starts here (as in yazi)
    fn start(&mut self, unpick: bool) {
        self.unpick = unpick;
        self.anchor = self.cur().map(|r| r.id.clone());
    }

    fn row(&self, i: &Item, w: usize) -> Line<'static> {
        let ago = crate::bill::ago(i.at as i64, (self.now)() as i64);
        let right = vec![Span::styled(ago, kit::dim())];
        let right_w: usize = right.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref())).sum();
        let room = w.saturating_sub(right_w + 3);
        // Group: name + how many sessions; session: title + which group it's in; several deleted together: a string of titles + how many
        let (name, what) = if i.group.is_empty() && i.sessions.len() > 1 {
            let titles: Vec<&str> = i.sessions.iter().map(|s| s.title.as_str()).collect();
            let what = tr!("{} 个会话", "{} sess.", i.sessions.len());
            let room = room.saturating_sub(unicode_width::UnicodeWidthStr::width(what.as_str()) + 2);
            (kit::clip(&titles.join(t!("、", ", ")), room).trim_end().to_string(), what)
        } else if i.group.is_empty() {
            let g = i.sessions.first().map(|s| s.group.as_str()).unwrap_or_default();
            (i.label().to_string(), if g.is_empty() { String::new() } else { crate::ui::group_label(g).to_string() })
        } else {
            let n = i.taken();
            (format!("▾ {}", i.group), if n > 0 { tr!("{n} 个会话", "{n} sess.") } else { String::new() })
        };
        let name = kit::clip(&name, room).trim_end().to_string();
        let what = kit::clip(&what, room.saturating_sub(unicode_width::UnicodeWidthStr::width(name.as_str()) + 2)).trim_end().to_string();
        let mut left = vec![Span::raw(" "), Span::styled(name, Style::new().add_modifier(Modifier::BOLD))];
        if !what.is_empty() {
            left.push(Span::styled(format!("  {what}"), kit::dim()));
        }
        kit::spread(left, right, w)
    }

    /// What's written below once d is pressed
    fn warning(&self, items: &[Item]) -> String {
        let n = items.len();
        let m: usize = items.iter().map(Item::taken).sum();
        match items {
            [i] if i.group.is_empty() || m == 0 => tr!("立即永久删除{}？", "Delete {} for good now?", i.name()),
            [i] => tr!("立即永久删除{}及其中 {m} 个会话？", "Delete {} and its {m} sessions for good now?", i.name()),
            // Every entry is one session: no need to also say how many sessions
            _ if items.iter().all(|i| i.group.is_empty() && i.sessions.len() == 1) => {
                tr!("立即永久删除这 {n} 条？", "Delete these {n} for good now?")
            }
            _ => tr!("立即永久删除这 {n} 条(共 {m} 个会话)？", "Delete these {n} for good now, {m} sessions in all?"),
        }
    }
}

/// One entry is just its own job; several are combined into one `Job::Many` (done one after another)
fn batch(items: Vec<Item>, job: impl Fn(Box<Item>) -> Job) -> Job {
    let mut jobs: Vec<Job> = items.into_iter().map(|i| job(Box::new(i))).collect();
    match jobs.len() {
        1 => jobs.remove(0),
        _ => Job::Many(jobs),
    }
}

impl Overlay for Page {
    fn width(&self) -> u16 {
        70
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        let n = self.rows.len();
        let ranging = self.anchor.is_some();
        let at = if n > SHOWN { vec![Span::styled(format!("{}/{n}  ", self.sel + 1), kit::dim())] } else { vec![] };
        let esc = match () {
            _ if self.armed => t!("取消", "keep"),
            _ if ranging => t!("确定", "keep"),
            _ if !self.marked.is_empty() => t!("清空选择", "clear"),
            _ => t!("关闭", "close"),
        };
        let mut out = vec![kit::modal_title(t!("回收站", "Trash"), theme::accent(), [at, kit::esc_hint(esc)].concat(), w), Line::raw("")];
        if self.rows.is_empty() {
            out.push(Line::styled(t!("回收站为空", "empty"), kit::dim()));
        }
        // Keep the highlight near the middle; selected rows as in the main list: a green "▌" at the start, on a pale green background
        let chosen: Vec<&str> = self.chosen().iter().map(|r| r.id.as_str()).collect();
        let from = self.sel.saturating_sub(SHOWN / 2).min(n.saturating_sub(SHOWN));
        for (i, r) in self.rows.iter().enumerate().skip(from).take(SHOWN) {
            let mut line = self.row(r, w);
            let on = chosen.contains(&r.id.as_str());
            if on {
                line.spans[0] = Span::styled("▌", Style::new().fg(theme::pal().green));
            }
            let bg = if i == self.sel { Some(theme::pal().sel) } else { on.then(|| theme::pal().pick) };
            out.push(kit::shaded(line, w, bg));
        }
        out.push(Line::raw(""));
        let Some(_) = self.cur() else { return out };
        if self.armed {
            out.extend(crate::ui::wrap_width(&self.warning(&self.targets()), w).into_iter().map(|l| Line::styled(l, theme::warn())));
            out.push(Line::raw(""));
            out.push(kit::keys_line(&[("y", t!("永久删除", "delete")), ("Esc", t!("取消", "keep"))], vec![], w));
            return out;
        }
        let acts = [("⏎", t!("恢复", "restore")), ("d", t!("删除", "delete"))];
        if !ranging && self.marked.is_empty() {
            out.push(kit::keys_line(&[acts[0], acts[1], ("v", t!("多选", "select"))], vec![], w));
            return out;
        }
        // While selecting: a green block up front says how many are selected (as in the main list's bottom bar)
        let count = tr!("已选 {}", "{} selected", chosen.len());
        let badge = match ranging {
            true if self.unpick => kit::button(&format!("{} · {count}", t!("取消选中", "unselect")), theme::pal().red),
            true => kit::button(&format!("{} · {count}", t!("连选", "range")), theme::pal().green),
            false => kit::button(&count, theme::pal().green),
        };
        // If it doesn't fit, drop from the end: Space first, then V (as in the main list's bottom bar)
        let on = self.on_marked();
        let keys = match ranging {
            true => vec![acts[0], acts[1]],
            false => vec![
                acts[0],
                acts[1],
                ("v", t!("连选", "range")),
                ("V", t!("连续取消", "range unselect")),
                (t!("空格", "Space"), if on { t!("取消选中", "unselect") } else { t!("选中", "select") }),
            ],
        };
        let used = unicode_width::UnicodeWidthStr::width(badge.content.as_ref()) + 1;
        let mut l = kit::keys_line(&keys, vec![], w.saturating_sub(used));
        l.spans.splice(0..0, [badge, Span::raw(" ")]);
        out.push(l);
        out
    }

    /// v select as in the main list (after yazi): v / V start a new range from this entry, Esc fixes a range in progress; when
    /// not ranging, Esc first clears the selection and only closes when pressed again;
    /// Space checks one and moves down (while ranging too)
    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        let n = self.rows.len();
        if std::mem::take(&mut self.armed) {
            match k.code {
                KeyCode::Char('y') => {
                    let gone = self.targets();
                    let ids: Vec<String> = gone.iter().map(|i| i.id.clone()).collect();
                    self.rows.retain(|r| !ids.contains(&r.id));
                    self.sel = self.sel.min(self.rows.len().saturating_sub(1));
                    self.dropped.extend(ids);
                    self.marked.clear();
                    self.anchor = None;
                    fx.job = Some(batch(gone, |item| Job::Purge { item }));
                }
                KeyCode::Esc => {}
                _ => fx.say(t!("未删除", "not deleted")),
            }
            return Flow::Stay;
        }
        let ranging = self.anchor.is_some();
        match k.code {
            KeyCode::Esc if ranging => {
                self.marked = self.chosen().iter().map(|r| r.id.clone()).collect();
                self.anchor = None;
            }
            // While ranging, no wrapping around to the top / bottom
            KeyCode::Down | KeyCode::Char('j') if ranging => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Up | KeyCode::Char('k') if ranging => self.sel = self.sel.saturating_sub(1),
            KeyCode::Esc if !self.marked.is_empty() => self.marked.clear(),
            KeyCode::Esc | KeyCode::Char('q') => return Flow::Close,
            KeyCode::Down | KeyCode::Char('j') if n > 0 => self.sel = (self.sel + 1) % n,
            KeyCode::Up | KeyCode::Char('k') if n > 0 => self.sel = (self.sel + n - 1) % n,
            KeyCode::Char(c @ ('v' | 'V')) if n > 0 => self.start(c == 'V'),
            KeyCode::Char(' ') if n > 0 => {
                let id = self.rows[self.sel].id.clone();
                match self.marked.iter().position(|m| *m == id) {
                    Some(i) => drop(self.marked.remove(i)),
                    None => self.marked.push(id),
                }
                self.sel = (self.sel + 1).min(n - 1);
            }
            KeyCode::Enter => {
                let items = self.targets();
                if !items.is_empty() {
                    fx.job = Some(batch(items, |item| Job::Restore { item, start: vec![] }));
                    return Flow::Close;
                }
            }
            KeyCode::Char('d' | 'x') if n > 0 => self.armed = true,
            _ => {}
        }
        Flow::Stay
    }

    fn reload(&mut self) {
        let was = self.cur().map(|i| i.id.clone());
        self.trash.reload();
        self.rows = self.trash.items((self.now)());
        // Ones just deleted for good and still being deleted in the background aren't read back
        self.rows.retain(|r| !self.dropped.contains(&r.id));
        self.sel = was.and_then(|id| self.rows.iter().position(|r| r.id == id)).unwrap_or(self.sel).min(self.rows.len().saturating_sub(1));
        // Ones restored / deleted for good elsewhere no longer count as selected
        let rows = &self.rows;
        self.marked.retain(|id| rows.iter().any(|r| r.id == *id));
        self.anchor = self.anchor.take().filter(|id| rows.iter().any(|r| r.id == *id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gone(id: &str, group: &str, archived: bool) -> Gone {
        Gone { id: id.into(), title: format!("标题{id}"), group: group.into(), archived }
    }

    fn item(id: &str, at: u64, group: &str, sessions: Vec<Gone>) -> Item {
        Item { id: id.into(), at, profile: "default".into(), group: group.into(), groups: vec![], sessions }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aca-trash-{name}-{}-{}", std::process::id(), aca_core::paths::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn two_admins_share_it_and_only_expired_ones_are_due() {
        let dir = temp("share");
        let path = dir.join("deleted");
        let mut desk = Trash::open(Some(path.clone()));
        let mut phone = Trash::open(Some(path.clone()));
        desk.add(item("1", 100, "", vec![gone("a", "fun", false)])).unwrap();
        desk.add(item("2", 200, "work", vec![gone("b", "work", false), gone("c", "work/x", true)])).unwrap();
        phone.reload();
        assert!(phone.has("default", "a") && phone.has("default", "c") && !phone.has("work", "a"));
        assert_eq!(phone.ids().collect::<Vec<_>>(), ["a", "b", "c"]);
        // Newest deleted first; expired ones aren't listed
        assert_eq!(phone.items(150).iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["2", "1"]);
        assert_eq!(phone.items(100 + KEEP).iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["2"]);
        assert!(!phone.due(100 + KEEP - 1) && phone.due(100 + KEEP));
        assert_eq!(phone.take_due(100 + KEEP).unwrap().iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["1"]);
        desk.reload();
        assert!(!desk.has("default", "a"));
        // Restored
        desk.forget("2").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_lines_are_dropped_and_defaults_fill_in() {
        let dir = temp("bad");
        std::fs::write(
            dir.join("deleted"),
            "x\n{\"id\":\"1\",\"at\":5,\"profile\":\"default\",\"sessions\":[{\"id\":\"a\",\"title\":\"t\"}]}\n",
        )
        .unwrap();
        let t = Trash::open(Some(dir.join("deleted")));
        assert_eq!(t.items, [item("1", 5, "", vec![Gone { id: "a".into(), title: "t".into(), group: String::new(), archived: false }])]);
        // Writing back also writes only what's there
        let line = serde_json::to_string(&t.items[0]).unwrap();
        assert_eq!(line, r#"{"id":"1","at":5,"profile":"default","sessions":[{"id":"a","title":"t"}]}"#);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_old_trash_moves_over_once_its_admin_is_gone() {
        let dir = temp("legacy");
        let old = dir.join("trash");
        let store = Store::memory();
        let mk = |id: &str, archived: u64| aca_core::store::Session {
            id: id.into(),
            title: id.into(),
            tool: "shell".into(),
            group: "fun".into(),
            dir: "/".into(),
            archived,
            ..Default::default()
        };
        for (id, archived) in [("a", 9), ("b", 9), ("c", 0), ("d", 9)] {
            store.insert(&mk(id, archived)).unwrap();
        }
        // a: its interface exited; b: that interface (pid 42) is gone; c: unarchived meanwhile; d: that interface (pid 7) is still open
        std::fs::write(&old, "0\tdefault\ta\n42\tdefault\tb\n0\tdefault\tc\n7\tdefault\td\n").unwrap();
        let mut bin = Trash::default();
        legacy(&old, &mut bin, &store, |p| p == 7).unwrap();
        let moved: Vec<(&str, &str)> = bin.items.iter().map(|i| (i.sessions[0].id.as_str(), i.sessions[0].group.as_str())).collect();
        assert_eq!(moved, [("a", "fun"), ("b", "fun")]);
        assert!(bin.items.iter().all(|i| i.group.is_empty() && !i.sessions[0].archived));
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "7\tdefault\td\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dropping_now_leaves_what_came_back_meanwhile() {
        let store = Store::memory();
        let mk = |id: &str, archived: u64| aca_core::store::Session {
            id: id.into(),
            title: id.into(),
            tool: "shell".into(),
            dir: "/nowhere".into(),
            archived,
            ..Default::default()
        };
        store.insert(&mk("a", 9)).unwrap();
        store.insert(&mk("b", 0)).unwrap();
        assert_eq!(drop_now(&store, &item("1", 1, "g", vec![gone("a", "g", false), gone("b", "g", false), gone("zz", "g", false)])), 1);
        assert!(store.get("a").is_err() && store.get("b").is_ok());
    }

    fn now() -> u64 {
        1_000_000
    }

    fn page() -> Page {
        let mut t = Trash::default();
        t.add(item("1", now() - 7200, "fun", vec![gone("a", "fun", false), gone("b", "fun/x", false), gone("c", "fun", true)])).unwrap();
        t.add(item("2", now() - 300, "", vec![gone("d", "work", false)])).unwrap();
        t.add(item("3", now() - KEEP, "", vec![gone("e", "", false)])).unwrap();
        Page::new(t, now)
    }

    fn text(p: &Page, w: usize) -> Vec<String> {
        p.lines(w).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    fn press(p: &mut Page, c: KeyCode) -> (Flow, Fx) {
        let mut fx = Fx::default();
        (p.on_key(KeyEvent::from(c), &mut fx), fx)
    }

    #[test]
    fn the_page_lists_what_was_deleted_newest_first() {
        let p = page();
        let rows = text(&p, 60);
        let at = |want: &str| rows.iter().position(|r| r.contains(want)).unwrap_or_else(|| panic!("缺「{want}」:{rows:#?}"));
        // A session shows its group; a group shows how many sessions (ones archived before the delete don't count);
        // expired ones aren't listed
        assert!(rows[at("标题d")].contains("work") && rows[at("标题d")].ends_with("5 分钟前"), "{rows:#?}");
        assert!(rows[at("▾ fun")].contains("2 个会话") && rows[at("▾ fun")].ends_with("2 小时前"), "{rows:#?}");
        assert!(at("标题d") < at("▾ fun"));
        assert!(!rows.iter().any(|r| r.contains("标题e")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("⏎ 恢复") && r.contains("d 删除")), "{rows:#?}");
        // Not too wide at phone width
        assert!(text(&p, 36).iter().all(|r| unicode_width::UnicodeWidthStr::width(r.as_str()) <= 36));
        let empty = Page::new(Trash::default(), now);
        assert!(text(&empty, 60).iter().any(|r| r == "回收站为空"));
    }

    #[test]
    fn sessions_deleted_together_are_one_row_named_after_the_first() {
        let mut t = Trash::default();
        let v = vec![gone("a", "fun", false), gone("b", "work", false), gone("c", "", false)];
        t.add(item("1", now() - 60, "", v)).unwrap();
        let it = t.items(now())[0].clone();
        assert_eq!(it.name(), "「标题a」等 3 个");
        // One row: a string of titles, the count always there (long titles get cut)
        let p = Page::new(t, now);
        let rows = text(&p, 60);
        assert!(rows.iter().any(|r| r.contains("标题a、标题b、标题c") && r.contains("3 个会话")), "{rows:#?}");
        assert!(text(&p, 36).iter().any(|r| r.contains("3 个会话")), "{:#?}", text(&p, 36));
        assert!(p.warning(std::slice::from_ref(&it)) == "立即永久删除「标题a」等 3 个？");
    }

    #[test]
    fn enter_restores_and_closes_d_then_y_deletes_for_good() {
        let mut p = page();
        let (flow, fx) = press(&mut p, KeyCode::Enter);
        assert_eq!(flow, Flow::Close);
        assert!(matches!(fx.job, Some(Job::Restore { ref item, ref start }) if item.id == "2" && start.is_empty()));
        // d: asks first, only y deletes
        let mut p = page();
        press(&mut p, KeyCode::Down);
        let (_, fx) = press(&mut p, KeyCode::Char('d'));
        assert!(fx.job.is_none() && p.armed);
        assert!(text(&p, 60).iter().any(|r| r == "立即永久删除分组 fun及其中 2 个会话？"), "{:#?}", text(&p, 60));
        let (flow, fx) = press(&mut p, KeyCode::Char('y'));
        assert_eq!(flow, Flow::Stay);
        assert!(matches!(fx.job, Some(Job::Purge { ref item }) if item.id == "1"));
        assert_eq!(p.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["2"]);
        assert_eq!(p.sel, 0);
        // Reload once done: removed ones don't come back
        p.reload();
        assert_eq!(p.rows.len(), 1);
        // Any other key: not deleted, and says so; Esc says nothing
        let (_, fx) = press(&mut p, KeyCode::Char('d'));
        assert!(fx.job.is_none());
        let (_, fx) = press(&mut p, KeyCode::Char('n'));
        assert_eq!((fx.job.is_none(), fx.flash.as_deref()), (true, Some("未删除")));
        press(&mut p, KeyCode::Char('d'));
        let (flow, fx) = press(&mut p, KeyCode::Esc);
        assert_eq!((flow, fx.flash), (Flow::Stay, None));
        assert!(!p.armed);
        assert_eq!(press(&mut p, KeyCode::Esc).0, Flow::Close);
    }

    /// Four entries, one deleted session each: 1 is the newest, at the top
    fn four() -> Page {
        let mut t = Trash::default();
        for (i, id) in ["1", "2", "3", "4"].into_iter().enumerate() {
            t.add(item(id, now() - 60 * (i as u64 + 1), "", vec![gone(&format!("s{id}"), "fun", false)])).unwrap();
        }
        Page::new(t, now)
    }

    fn chosen(p: &Page) -> Vec<&str> {
        p.chosen().iter().map(|r| r.id.as_str()).collect()
    }

    fn keys(p: &mut Page, ks: &str) {
        ks.chars().for_each(|c| drop(press(p, KeyCode::Char(c))));
    }

    /// How wide the trash's inside is on desktop: 70 minus border and padding
    fn has(p: &Page, want: &[&str]) {
        let rows = text(p, 64);
        for w in want {
            assert!(rows.iter().any(|r| r.contains(w)), "缺「{w}」:{rows:#?}");
        }
    }

    #[test]
    fn v_selects_like_the_list_and_esc_steps_back_one_level() {
        let mut p = four();
        has(&p, &["⏎ 恢复", "d 删除", "v 多选", "Esc 关闭"]);
        // v drags a range: selected rows start with "▌"; v again mid-range: the unfixed range is dropped and a new one starts here
        keys(&mut p, "vj");
        assert_eq!(chosen(&p), ["1", "2"]);
        has(&p, &["连选 · 已选 2", "Esc 确定"]);
        assert_eq!(text(&p, 60).iter().filter(|r| r.starts_with('▌')).count(), 2);
        keys(&mut p, "v");
        assert_eq!(chosen(&p), ["2"]);
        // Esc fixes it; highlight on a checked entry: Space unchecks, v range-selects, V range-unchecks
        keys(&mut p, "k");
        press(&mut p, KeyCode::Esc);
        assert_eq!(chosen(&p), ["1", "2"]);
        has(&p, &["已选 2", "空格 取消选中", "v 连选", "V 连续取消", "Esc 清空选择"]);
        // Phone width: if it doesn't fit, Space goes first, then V (as in the main list's bottom bar)
        let phone = text(&p, 40);
        assert!(phone.iter().any(|r| r.contains("v 连选") && !r.contains("V 连") && !r.contains("空格")), "{phone:#?}");
        assert!(phone.iter().all(|r| unicode_width::UnicodeWidthStr::width(r.as_str()) <= 40));
        // Space checks one and moves down
        keys(&mut p, "jj ");
        assert_eq!((chosen(&p), p.sel), (vec!["1", "2", "3"], 3));
        // v from a checked entry still range-selects; V range-unchecks
        keys(&mut p, "kkvj");
        assert_eq!(chosen(&p), ["1", "2", "3"]);
        press(&mut p, KeyCode::Esc);
        keys(&mut p, "Vk");
        assert_eq!(chosen(&p), ["1"]);
        has(&p, &["取消选中 · 已选 1"]);
        // V / v again mid-range: the unfixed range is dropped, and a new one of that kind starts here
        keys(&mut p, "V");
        assert_eq!(chosen(&p), ["1", "3"]);
        keys(&mut p, "v");
        assert_eq!(chosen(&p), ["1", "2", "3"]);
        // Space mid-range: checks / unchecks this one and moves down, still ranging
        keys(&mut p, " ");
        assert_eq!((chosen(&p), p.sel, p.anchor.is_some()), (vec!["1", "2", "3"], 2, true));
        press(&mut p, KeyCode::Esc);
        assert_eq!(chosen(&p), ["1", "2", "3"]);
        // Esc: first clears the selection, only closes when pressed again; V works with nothing checked (as in yazi), Esc gets out
        assert_eq!(press(&mut p, KeyCode::Esc).0, Flow::Stay);
        assert!(chosen(&p).is_empty());
        keys(&mut p, "V");
        has(&p, &["取消选中 · 已选 0"]);
        assert_eq!(press(&mut p, KeyCode::Esc).0, Flow::Stay);
        assert_eq!(press(&mut p, KeyCode::Esc).0, Flow::Close);
    }

    #[test]
    fn enter_restores_the_selected_together_and_d_y_deletes_them_for_good() {
        let id = |j: &Job| match j {
            Job::Restore { item, .. } => item.id.clone(),
            j => panic!("{j:?}"),
        };
        let restores = |fx: &Fx| match &fx.job {
            Some(Job::Many(v)) => v.iter().map(id).collect::<Vec<_>>(),
            Some(j) => vec![id(j)],
            None => panic!("没有活"),
        };
        // Three in a range: ⏎ restores them together and the page closes
        let mut p = four();
        keys(&mut p, "vjj");
        let (flow, fx) = press(&mut p, KeyCode::Enter);
        assert_eq!((flow, restores(&fx)), (Flow::Close, vec!["1".to_string(), "2".into(), "3".into()]));
        // Only one checked: just that one (wherever the highlight is)
        let mut p = four();
        keys(&mut p, "jj ");
        let (_, fx) = press(&mut p, KeyCode::Enter);
        assert!(matches!(fx.job, Some(Job::Restore { ref item, .. }) if item.id == "3"));
        // Space checks two, d asks, saying how many, y deletes them together for good
        let mut p = four();
        keys(&mut p, "  d");
        has(&p, &["立即永久删除这 2 条？", "y 永久删除"]);
        assert!(!text(&p, 60).iter().any(|r| r.contains("清空选择")), "{:#?}", text(&p, 60));
        let (_, fx) = press(&mut p, KeyCode::Char('y'));
        let Some(Job::Many(v)) = fx.job else { panic!() };
        assert!(matches!(&v[..], [Job::Purge { item: a }, Job::Purge { item: b }] if a.id == "1" && b.id == "2"));
        assert_eq!(p.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["3", "4"]);
        assert!(p.marked.is_empty());
        p.reload();
        assert_eq!(p.rows.len(), 2);
        // With a group in it: says how many sessions in all
        let mut p = page();
        keys(&mut p, "vjd");
        has(&p, &["立即永久删除这 2 条(共 3 个会话)？"]);
    }
}
