//! 回收站:`d` 删掉的会话、删掉的分组(连子分组、里面的会话)在这里留 1 天,之内 `b` 那一页 `⏎` 拿回来,
//! 刚删的 `^Z` 也撤得回(见 undo.rs);过了 1 天才真删。一次删除是一条:删一个组连里面的会话也是一条,一起回来。
//!
//! 删 = 归档(程序和 tmux 会话停掉,登记和对话都留着),分组的登记删掉、照原样记在这里;
//! 拿回来 = 照记的建回分组、取消归档(见 ops.rs 的 `Job::Restore`)。
//! 真删是打开、开着(每次刷新看一眼)、退出的界面顺手做的(aca 没有常驻的进程):只会晚,不会早。
//! 还归档着才真删:期间用 `aca unarchive` 取消了归档的,说明又想要了,不删。
//!
//! 存在 ~/.local/state/aca/ui/deleted,一行一条(JSON)。和置顶一样每次刷新都重读、改之前先重读,手机和桌面各开一个界面都一样。
//! 以前的版本记在同一个目录的 trash 里(`<删它的界面的 pid>\t<profile>\t<会话 id>`,界面退出就真删):
//! 删它的界面已经退出的搬过来,从搬的时候起留 1 天;还开着的旧界面自己的那些留给它。

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

/// 删掉的留这么久(秒)
pub const KEEP: u64 = 24 * 3600;

/// 一页最多列这么多条,再多跟着高亮滚
const SHOWN: usize = 12;

/// 一次删除
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// 这一条的键
    pub id: String,
    /// 什么时候删的(秒)
    pub at: u64,
    pub profile: String,
    /// 删的是哪个分组;空 = 删的是一个会话
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    /// 一起删掉的分组(它自己和子分组),照原样建回去
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<GroupRec>,
    /// 一起删掉的会话
    #[serde(default)]
    pub sessions: Vec<Gone>,
}

/// 删掉的分组在登记表里原来的样子
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupRec {
    pub path: String,
    /// 登记表里的先后
    pub order: i64,
    /// 默认目录
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dir: String,
    /// 记在哪个工作区(界面的,见 spaces.rs)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<char>,
    /// 它的便签(界面的,见 notes.rs)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// 删掉的一个会话
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gone {
    pub id: String,
    pub title: String,
    /// 在哪个分组(空 = 没分组)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    /// 删之前就归档着:拿回来还归档着
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
}

impl Item {
    /// 删一个会话的那一条
    pub fn session(profile: &str, s: Gone) -> Item {
        Item::sessions(profile, vec![s])
    }

    /// v 多选一起删的几个会话:一条,一起回来
    pub fn sessions(profile: &str, v: Vec<Gone>) -> Item {
        Item { id: new_id(), at: aca_core::paths::now(), profile: profile.into(), group: String::new(), groups: vec![], sessions: v }
    }

    /// 回收站页、提示里叫它什么:分组叫组名,会话叫标题
    pub fn label(&self) -> &str {
        match self.sessions.first() {
            Some(s) if self.group.is_empty() => &s.title,
            _ => &self.group,
        }
    }

    /// 提示里叫它什么:会话是「标题」(长的截掉),一起删的几个是「第一个」等 n 个,分组是「分组 名字」。前面不用空一格(「已恢复{}」)
    pub fn name(&self) -> String {
        if self.group.is_empty() {
            crate::ops::named(self.label(), self.sessions.len())
        } else {
            let short: String = self.group.chars().take(16).collect();
            let more = if self.group.chars().count() > 16 { "…" } else { "" };
            tr!("分组 {short}{more}", "group {short}{more}")
        }
    }

    /// 拿回来时要取消归档的(删之前就归档着的不算)有几个
    pub fn taken(&self) -> usize {
        self.sessions.iter().filter(|s| !s.archived).count()
    }

    fn expired(&self, now: u64) -> bool {
        self.at + KEEP <= now
    }
}

/// 一条的键:时刻 + 随机几位
pub fn new_id() -> String {
    format!("{}-{}", aca_core::paths::now_ms(), aca_core::paths::short_id())
}

#[derive(Clone, Default)]
pub struct Trash {
    /// None = 只在内存里(单元测试、--snapshot)
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

    /// 读完就和文件断开,之后的改动只在内存里
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

    /// 这个会话在回收站里
    pub fn has(&self, profile: &str, id: &str) -> bool {
        self.items.iter().any(|i| i.profile == profile && i.sessions.iter().any(|s| s.id == id))
    }

    /// 回收站里全部会话的 id
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.items.iter().flat_map(|i| i.sessions.iter().map(|s| s.id.as_str()))
    }

    pub fn get(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// 还没到期的,新删的在前
    pub fn items(&self, now: u64) -> Vec<Item> {
        let mut v: Vec<Item> = self.items.iter().filter(|i| !i.expired(now)).cloned().collect();
        v.sort_by_key(|i| std::cmp::Reverse(i.at));
        v
    }

    /// 有到期该真删的
    pub fn due(&self, now: u64) -> bool {
        self.items.iter().any(|i| i.expired(now))
    }

    pub fn add(&mut self, item: Item) -> std::io::Result<()> {
        self.reload();
        self.items.retain(|i| i.id != item.id);
        self.items.push(item);
        self.save()
    }

    /// 拿回来了 / 真删了
    pub fn forget(&mut self, id: &str) -> std::io::Result<()> {
        self.reload();
        self.items.retain(|i| i.id != id);
        self.save()
    }

    /// 到期的从文件里拿走
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

/// 那个 pid 的进程还在(发 0 号信号试试)
fn alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// 这一条里还归档着的会话现在就真删。返回删了几个
pub fn drop_now(store: &Store, item: &Item) -> usize {
    let still = |id: &str| store.get(id).is_ok_and(|s| s.id == id && s.archived > 0);
    item.sessions.iter().filter(|s| still(&s.id) && aca_core::session::remove(store, &s.id).is_ok()).count()
}

/// 真删到期的(先把旧版记的搬过来)
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

/// 旧版的 trash:删它的界面已经退出的(pid 0 / 进程不在了)、还归档着的,搬进回收站,从现在起留 1 天
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

/// 界面退出时:起一个 `aca ui --purge` 去真删到期的。放进自己的进程组、不接终端,弹窗关了 / 终端断了也照样删完
pub fn purge_detached() {
    let Some(me) = aca_core::agent::self_path() else { return };
    let _ =
        aca_core::proc::detach(Command::new(me).args(["ui", "--purge"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()));
}

/// `b`:回收站。一行一条删掉的,新删的在上;`⏎` 拿回来(会话是停着的),`d` 再 `y` 现在就真删。
/// `v` 多选同主列表(同 yazi):`v` 连选、`V` 连着去掉勾、空格勾一条,`⏎` / `d` 对着选上的一起做
pub struct Page {
    trash: Trash,
    now: fn() -> u64,
    rows: Vec<Item>,
    sel: usize,
    /// 按了 d,等 y
    armed: bool,
    /// 这一页里按了真删的
    dropped: Vec<String>,
    /// 勾上的(条的 id)
    marked: Vec<String>,
    /// v / V 连选:从这一条(id)连到高亮那行;Esc 定下
    anchor: Option<String>,
    /// 这一段是 V 连着去掉勾
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

    /// 连选着的那一段:从起点那条到高亮这条
    fn range(&self) -> Option<std::ops::RangeInclusive<usize>> {
        let a = self.rows.iter().position(|r| self.anchor.as_ref() == Some(&r.id))?;
        Some(a.min(self.sel)..=a.max(self.sel))
    }

    /// 选上的:勾上的加上连选着的(去掉勾的那种连选:减去),按页上的先后
    fn chosen(&self) -> Vec<&Item> {
        let range = self.range();
        let ranged = |i: usize| range.as_ref().is_some_and(|r| r.contains(&i));
        let on = |i: usize, r: &Item| match self.unpick {
            true => self.marked.contains(&r.id) && !ranged(i),
            false => self.marked.contains(&r.id) || ranged(i),
        };
        self.rows.iter().enumerate().filter(|&(i, r)| on(i, r)).map(|(_, r)| r).collect()
    }

    /// ⏎ / d 对着的:选上了就是选上的,没选就是高亮的那条
    fn targets(&self) -> Vec<Item> {
        match self.chosen() {
            c if c.is_empty() => self.cur().cloned().into_iter().collect(),
            c => c.into_iter().cloned().collect(),
        }
    }

    /// 高亮这条勾着没有:空格按下去是勾上还是去掉
    fn on_marked(&self) -> bool {
        self.cur().is_some_and(|r| self.marked.contains(&r.id))
    }

    /// v 从高亮这条连选,V(unpick)连着去掉勾;连选着再按:没定下的那段不要了,从这条重新起(同 yazi)
    fn start(&mut self, unpick: bool) {
        self.unpick = unpick;
        self.anchor = self.cur().map(|r| r.id.clone());
    }

    fn row(&self, i: &Item, w: usize) -> Line<'static> {
        let ago = crate::bill::ago(i.at as i64, (self.now)() as i64);
        let right = vec![Span::styled(ago, kit::dim())];
        let right_w: usize = right.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref())).sum();
        let room = w.saturating_sub(right_w + 3);
        // 分组:组名 + 几个会话;会话:标题 + 它在哪个组;一起删的几个:一串标题 + 几个
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

    /// 按了 d 之后写在下面的
    fn warning(&self, items: &[Item]) -> String {
        let n = items.len();
        let m: usize = items.iter().map(Item::taken).sum();
        match items {
            [i] if i.group.is_empty() || m == 0 => tr!("立即永久删除{}？", "Delete {} for good now?", i.name()),
            [i] => tr!("立即永久删除{}及其中 {m} 个会话？", "Delete {} and its {m} sessions for good now?", i.name()),
            // 都是一条一个会话:不用再说几个会话
            _ if items.iter().all(|i| i.group.is_empty() && i.sessions.len() == 1) => {
                tr!("立即永久删除这 {n} 条？", "Delete these {n} for good now?")
            }
            _ => tr!("立即永久删除这 {n} 条(共 {m} 个会话)？", "Delete these {n} for good now, {m} sessions in all?"),
        }
    }
}

/// 一条就是它自己那个活,几条合成一个 `Job::Many`(一条接一条做)
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
        // 高亮尽量在中间;选上的同主列表:行首一道绿「▌」、铺淡绿底
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
        // 多选着:打头一块绿底写选上几条(同主列表的底栏)
        let count = tr!("已选 {}", "{} selected", chosen.len());
        let badge = match ranging {
            true if self.unpick => kit::button(&format!("{} · {count}", t!("取消选中", "unselect")), theme::pal().red),
            true => kit::button(&format!("{} · {count}", t!("连选", "range")), theme::pal().green),
            false => kit::button(&count, theme::pal().green),
        };
        // 放不下从后往前丢:先丢空格、再丢 V(同主列表的底栏)
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

    /// 同主列表的 v 多选(照 yazi):v / V 从这条重新连一段,连选着 Esc 定下;没在连选 Esc 先全不选,再按才关;
    /// 空格勾一条、往下走(连选着也是)
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
            // 连选着不绕回顶上 / 底下
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
        // 刚按了真删、后台还在删的不再读回来
        self.rows.retain(|r| !self.dropped.contains(&r.id));
        self.sel = was.and_then(|id| self.rows.iter().position(|r| r.id == id)).unwrap_or(self.sel).min(self.rows.len().saturating_sub(1));
        // 别处拿回来 / 真删了的不再算选上
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
        // 新删的在前;到期的不列
        assert_eq!(phone.items(150).iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["2", "1"]);
        assert_eq!(phone.items(100 + KEEP).iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["2"]);
        assert!(!phone.due(100 + KEEP - 1) && phone.due(100 + KEEP));
        assert_eq!(phone.take_due(100 + KEEP).unwrap().iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["1"]);
        desk.reload();
        assert!(!desk.has("default", "a"));
        // 拿回来了
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
        // 写回去也只写有的
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
        // a:界面退出了;b:那个界面(pid 42)不在了;c:期间取消归档了;d:那个界面(pid 7)还开着
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
        // 会话写它在哪个组;分组写几个会话(删之前就归档着的不算);到期的不列
        assert!(rows[at("标题d")].contains("work") && rows[at("标题d")].ends_with("5 分钟前"), "{rows:#?}");
        assert!(rows[at("▾ fun")].contains("2 个会话") && rows[at("▾ fun")].ends_with("2 小时前"), "{rows:#?}");
        assert!(at("标题d") < at("▾ fun"));
        assert!(!rows.iter().any(|r| r.contains("标题e")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("⏎ 恢复") && r.contains("d 删除")), "{rows:#?}");
        // 手机宽度不超宽
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
        // 一行:一串标题,个数总在(标题长了截标题)
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
        // d:问一句,y 才删
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
        // 做完重读:拿掉的不回来
        p.reload();
        assert_eq!(p.rows.len(), 1);
        // 别的键:不删,说一声;Esc 不说
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

    /// 四条,各删了一个会话:1 最新,排在最上面
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

    /// 桌面上回收站里面有多宽:70 减去边框和留白
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
        // v 拉一段:选上的行首一道「▌」;连选着再按 v:没定下的这一段不要了,从这条重新起
        keys(&mut p, "vj");
        assert_eq!(chosen(&p), ["1", "2"]);
        has(&p, &["连选 · 已选 2", "Esc 确定"]);
        assert_eq!(text(&p, 60).iter().filter(|r| r.starts_with('▌')).count(), 2);
        keys(&mut p, "v");
        assert_eq!(chosen(&p), ["2"]);
        // Esc 确定;高亮在勾着的那条:空格是去掉,v 连选、V 连着去掉勾
        keys(&mut p, "k");
        press(&mut p, KeyCode::Esc);
        assert_eq!(chosen(&p), ["1", "2"]);
        has(&p, &["已选 2", "空格 取消选中", "v 连选", "V 连续取消", "Esc 清空选择"]);
        // 手机宽度:放不下先丢空格,再丢 V(同主列表的底栏)
        let phone = text(&p, 40);
        assert!(phone.iter().any(|r| r.contains("v 连选") && !r.contains("V 连") && !r.contains("空格")), "{phone:#?}");
        assert!(phone.iter().all(|r| unicode_width::UnicodeWidthStr::width(r.as_str()) <= 40));
        // 空格勾一条、往下走
        keys(&mut p, "jj ");
        assert_eq!((chosen(&p), p.sel), (vec!["1", "2", "3"], 3));
        // 从勾着的那条起 v 还是连选;V 连着去掉勾
        keys(&mut p, "kkvj");
        assert_eq!(chosen(&p), ["1", "2", "3"]);
        press(&mut p, KeyCode::Esc);
        keys(&mut p, "Vk");
        assert_eq!(chosen(&p), ["1"]);
        has(&p, &["取消选中 · 已选 1"]);
        // 连选着再按 V / v:没定下的那段不要了,从这条起重新连那一种
        keys(&mut p, "V");
        assert_eq!(chosen(&p), ["1", "3"]);
        keys(&mut p, "v");
        assert_eq!(chosen(&p), ["1", "2", "3"]);
        // 连选着空格:勾上 / 去掉这一条、往下走,还在连选
        keys(&mut p, " ");
        assert_eq!((chosen(&p), p.sel, p.anchor.is_some()), (vec!["1", "2", "3"], 2, true));
        press(&mut p, KeyCode::Esc);
        assert_eq!(chosen(&p), ["1", "2", "3"]);
        // Esc:先全不选,再按才关;什么都没勾也能 V(同 yazi),Esc 出来
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
        // 连着的三条:⏎ 一起拿回来,页关掉
        let mut p = four();
        keys(&mut p, "vjj");
        let (flow, fx) = press(&mut p, KeyCode::Enter);
        assert_eq!((flow, restores(&fx)), (Flow::Close, vec!["1".to_string(), "2".into(), "3".into()]));
        // 只勾上一条:就是它自己(不管高亮在哪)
        let mut p = four();
        keys(&mut p, "jj ");
        let (_, fx) = press(&mut p, KeyCode::Enter);
        assert!(matches!(fx.job, Some(Job::Restore { ref item, .. }) if item.id == "3"));
        // 空格勾两条,d 问一句写几条,y 一起真删
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
        // 有分组的:写一共几个会话
        let mut p = page();
        keys(&mut p, "vjd");
        has(&p, &["立即永久删除这 2 条(共 3 个会话)？"]);
    }
}
