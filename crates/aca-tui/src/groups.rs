//! 删分组:列表里停在分组上按 `d` 开这一页 —— 登记表里全部的分组,没有会话的也列(列表里看不到它们),
//! 高亮停在刚才那个组上,再按一次 `d` 删。
//! 改分组:停在分组上按 `r`,先改名(可以带上级:`fun/aca`),再改默认目录(`Act::Edit`,见 `rename_all`)。
//!
//! 删是登记表的 `group_remove`:
//! - 子分组跟着一起删
//! - 里面的会话(连子分组里的、归档着的)都挪到上级分组,顶层的变成没分组的(列在「未分组」下面)。会话本身不动,照样跑着
//! - 归档着的也算「里面有会话」:界面里 `d` 删的会话在界面退出前都还归档着(等 `^Z`),所以列表里看着空了的组可能还不空
//!
//! 确认时看到的是空的(`force = false`),动手时里面却有会话了(中间别处往里加的):不删,照实说,不替人挪。
//! 会话挪到上级以后还留在原来的工作区、组在工作区里的记录清掉,是界面那边做的(`ui::stay_on_group` / `job_done`)。

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use aca_core::paths::Res;
use aca_core::store::Store;
use aca_core::{t, tr};

use crate::ops::{Job, Outcome};
use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::theme;

/// 顶层分组删掉后,里面的会话列在哪(没分组的)
const DEFAULT: &str = crate::data::UNGROUPED;

/// 一页最多列这么多个分组,再多跟着高亮滚
const SHOWN: usize = 14;

/// 登记表里的一个分组
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub profile: String,
    /// 分组路径(`上级/名字`)
    pub path: String,
    /// 直接在这个组里的会话:没归档的
    pub live: usize,
    /// 归档着的
    pub archived: usize,
}

/// g 是 top 自己或者它下面的
fn within(g: &str, top: &str) -> bool {
    g == top || g.strip_prefix(top).is_some_and(|r| r.starts_with('/'))
}

/// 分组 g 改名(from → to)以后叫什么:是 from 自己或者它下面的才改,别的 None
pub fn renamed(g: &str, from: &str, to: &str) -> Option<String> {
    within(g, from).then(|| format!("{to}{}", &g[from.len()..]))
}

/// 改分组的路径(`aca group rename`、分组上 `r`):登记表里的组和会话,连界面记着组名的
/// (工作区里记的组、视图里在看的组和折起来的)一起改。返回挪了几个会话
pub fn rename_all(store: &Store, from: &str, to: &str) -> Res<usize> {
    let n = store.group_rename(from, to)?;
    // 界面记的没改上不算失败:最多是这个组回到默认的工作区、折着的展开
    let _ = crate::spaces::Spaces::open(crate::spaces::Spaces::default_path()).rename_group(crate::data::REGISTERED, from, to);
    if let Some(p) = crate::state::state_file("view") {
        let mut v = crate::state::View::load(&p);
        if v.rename_group(from, to) {
            let _ = v.save(&p);
        }
    }
    Ok(n)
}

/// 要做的
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// 删掉这个分组(子分组一起);force = 里面有会话,挪到上级再删
    Delete { profile: String, group: String, force: bool },
    /// 分组上 `r`:group 改成 to(一样 = 不改名);dir:默认目录 (原来的, 改成的),空 = 不记;None = 不改
    Edit { profile: String, group: String, to: String, dir: Option<(String, String)> },
}

impl Act {
    /// 跑的时候底栏显示的
    pub fn doing(&self) -> String {
        match self {
            Act::Delete { group, .. } => tr!("正在删分组 {group}…", "deleting group {group}…"),
            Act::Edit { group, .. } => tr!("正在改分组 {group}…", "changing group {group}…"),
        }
    }

    /// 这个分组会不会被删掉(它自己和子分组)
    pub fn covers(&self, profile: &str, group: &str) -> bool {
        match self {
            Act::Delete { profile: p, group: top, .. } => p == profile && within(group, top),
            Act::Edit { .. } => false,
        }
    }

    /// 里面的会话挪去哪:上级分组,顶层的列在「未分组」下面
    pub fn dest(&self) -> String {
        match self {
            Act::Delete { group, .. } => dest(group),
            Act::Edit { to, .. } => to.clone(),
        }
    }

    /// 改分组的反过来做(^Z)
    pub fn undo(&self) -> Option<Act> {
        match self {
            Act::Delete { .. } => None,
            Act::Edit { profile, group, to, dir } => Some(Act::Edit {
                profile: profile.clone(),
                group: to.clone(),
                to: group.clone(),
                dir: dir.as_ref().map(|(was, now)| (now.clone(), was.clone())),
            }),
        }
    }
}

fn dest(group: &str) -> String {
    group.rsplit_once('/').map_or(DEFAULT, |x| x.0).to_string()
}

/// run 循环放进后台跑的(`Job::Group`)
pub fn job(a: &Act) -> Outcome {
    let (group, force) = match a {
        Act::Delete { group, force, .. } => (group, *force),
        Act::Edit { group, to, dir, .. } => return edit(group, to, dir.as_ref().map(|d| d.1.as_str())),
    };
    let rows = load();
    if !rows.iter().any(|r| r.path == *group) {
        return Outcome::Done(tr!("分组 {group} 本来就没有了", "group {group} was already gone"));
    }
    let inside: usize = rows.iter().filter(|r| within(&r.path, group)).map(|r| r.live + r.archived).sum();
    if inside > 0 && !force {
        return Outcome::Failed(tr!("没删:{group} 里现在有会话了", "not deleted: {group} has sessions now"));
    }
    match Store::open().and_then(|s| s.group_remove(group)) {
        Ok(n) => Outcome::Done(done(group, n)),
        Err(e) => Outcome::Failed(tr!("删分组失败:{e}", "deleting the group failed: {e}")),
    }
}

/// 改名、改默认目录。目录先查(撞了别的组的就都不改),再改名,再记目录
fn edit(group: &str, to: &str, dir: Option<&str>) -> Outcome {
    let store = match Store::open() {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(tr!("改分组失败:{e}", "changing the group failed: {e}")),
    };
    if let Some(d) = dir.filter(|d| !d.is_empty())
        && let Some(g) =
            store.groups().unwrap_or_default().into_iter().find(|g| {
                g.path != group && !g.default_path.is_empty() && aca_core::paths::real(&g.default_path) == aca_core::paths::real(d)
            })
    {
        return Outcome::Failed(tr!(
            "没改:{} 已经是分组 {} 的默认目录了",
            "unchanged: {} is already the default directory of group {}",
            crate::create::tilde(d),
            g.path
        ));
    }
    let mut said = vec![];
    if to != group {
        if let Err(e) = rename_all(&store, group, to) {
            return Outcome::Failed(tr!("改分组名失败:{e}", "renaming the group failed: {e}"));
        }
        said.push(tr!("分组 {group} 改名为 {to}", "group {group} renamed to {to}"));
    }
    if let Some(d) = dir {
        if let Err(e) = store.group_set_dir(to, d) {
            return Outcome::Failed(tr!(
                "{}默认目录没改上:{e}",
                "{}default directory not changed: {e}",
                if said.is_empty() { "" } else { t!("改了名,", "renamed, ") }
            ));
        }
        let d = if d.is_empty() { t!("不记了", "forgotten").to_string() } else { crate::create::tilde(d) };
        said.push(if said.is_empty() {
            tr!("分组 {to} 的默认目录:{d}", "default directory of group {to}: {d}")
        } else {
            tr!("默认目录:{d}", "default directory: {d}")
        });
    }
    Outcome::Done(said.join(" · "))
}

/// 删成了说什么:挪走了会话的写上几个、去了哪
fn done(group: &str, moved: usize) -> String {
    if moved > 0 {
        tr!(
            "已删分组 {group} · {moved} 个会话挪到 {}",
            "deleted group {group} · sessions moved to {}: {moved}",
            crate::ui::group_label(&dest(group))
        )
    } else {
        tr!("已删分组 {group}", "deleted group {group}")
    }
}

/// 登记表里的分组,按它的树排(父组紧跟着子组,同级按 sort_order 再按名字),带上组里有几个会话
pub fn load() -> Vec<Row> {
    let Ok(store) = Store::open() else { return vec![] };
    rows_of(&store)
}

fn rows_of(store: &Store) -> Vec<Row> {
    let order: HashMap<String, i64> = store.groups().unwrap_or_default().into_iter().map(|g| (g.path, g.order)).collect();
    let mut counts: HashMap<String, (usize, usize)> = HashMap::new();
    for s in store.sessions(true).unwrap_or_default() {
        let c = counts.entry(s.group).or_default();
        if s.archived > 0 { c.1 += 1 } else { c.0 += 1 }
    }
    let ranks = crate::data::group_ranks(order.keys().map(String::as_str), &order);
    let mut paths: Vec<&String> = order.keys().collect();
    paths.sort_by_key(|p| ranks[p.as_str()]);
    paths
        .into_iter()
        .map(|p| {
            let (live, archived) = counts.get(p).copied().unwrap_or((0, 0));
            Row { profile: crate::data::REGISTERED.into(), path: p.clone(), live, archived }
        })
        .collect()
}

/// 分组上 `d` 的那一页:一行一个分组,d 删(按两次)
pub struct Page {
    load: fn() -> Vec<Row>,
    /// 不列的:(profile, 分组)。藏在隐藏工作区里的组,人不在那边时不漏出来
    hide: Vec<(String, String)>,
    /// 库里全部的,藏起来的也在:数「删掉它会带走什么」用
    all: Vec<Row>,
    /// 列出来的
    rows: Vec<Row>,
    sel: usize,
    /// 按了一次 d,等再按一次
    armed: bool,
}

impl Page {
    /// at:(profile, 分组),高亮停在它上面,等着再按一次 d;页面里没有它(不是登记的分组)就停在第一行
    pub fn new(load: fn() -> Vec<Row>, hide: Vec<(String, String)>, at: (&str, &str)) -> Page {
        let mut p = Page { load, hide, all: vec![], rows: vec![], sel: 0, armed: false };
        p.reload();
        let found = p.rows.iter().position(|r| r.profile == at.0 && r.path == at.1).or_else(|| p.rows.iter().position(|r| r.path == at.1));
        if let Some(i) = found {
            p.sel = i;
            p.armed = true;
        }
        p
    }

    fn cur(&self) -> Option<&Row> {
        self.rows.get(self.sel)
    }

    /// 删掉 r 会带走的:(子分组几个, 会话几个, 其中归档着的几个),子分组里的也算
    fn load_of(&self, r: &Row) -> (usize, usize, usize) {
        let inside: Vec<&Row> = self.rows_of(r).collect();
        let (live, arch) = inside.iter().fold((0, 0), |(l, a), x| (l + x.live, a + x.archived));
        (inside.len() - 1, live + arch, arch)
    }

    /// r 和它下面的分组。藏起来的也算:它们一样会被删
    fn rows_of<'a>(&'a self, r: &'a Row) -> impl Iterator<Item = &'a Row> {
        self.all.iter().filter(move |x| x.profile == r.profile && within(&x.path, &r.path))
    }

    /// 按了一次 d 之后写在下面的:删掉它会怎样。一句一行(名字长的时候不在半中间折断)
    fn warning(&self, r: &Row) -> Vec<String> {
        let (kids, n, arch) = self.load_of(r);
        let mut out = vec![tr!("再按 d 删掉 {}", "press d again to delete {}", r.path)];
        if n > 0 {
            let arch = if arch > 0 { tr!("({arch} 个是归档的)", " ({arch} archived)") } else { String::new() };
            out.push(tr!("里面 {n} 个会话{arch}挪到 {}", "its sessions move to {}: {n}{arch}", crate::ui::group_label(&dest(&r.path))));
        }
        if kids > 0 {
            out.push(tr!("{kids} 个子分组一起删", "subgroups deleted with it: {kids}"));
        }
        if out.len() == 1 {
            out[0].push_str(t!("(空的)", " (empty)"));
        }
        out
    }

    fn row(&self, r: &Row, w: usize) -> Line<'static> {
        let count = match (r.live, r.archived) {
            (0, 0) => t!("空", "empty").to_string(),
            (0, a) => tr!("{a} 个归档", "{a} archived"),
            (n, 0) => tr!("{n} 个会话", "{n} sess."),
            (n, a) => tr!("{n} 个会话 · {a} 个归档", "{n} sess. · {a} archived"),
        };
        let right = vec![Span::styled(count, if r.live > 0 { Style::new() } else { kit::dim() })];
        let right_w: usize = right.iter().map(|s| s.content.width()).sum();
        let name = kit::clip(&r.path, w.saturating_sub(right_w + 3)).trim_end().to_string();
        kit::spread(vec![Span::raw(" "), Span::styled(name, Style::new().add_modifier(Modifier::BOLD))], right, w)
    }
}

impl Overlay for Page {
    fn width(&self) -> u16 {
        64
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        let wrap = |text: &str, st: Style| crate::ui::wrap_width(text, w).into_iter().map(move |l| Line::styled(l, st)).collect::<Vec<_>>();
        let n = self.rows.len();
        let at = if n > SHOWN { vec![Span::styled(format!("{}/{n}  ", self.sel + 1), kit::dim())] } else { vec![] };
        let mut out = vec![
            kit::modal_title(t!("分组", "Groups"), theme::accent(), [at, kit::esc_hint(t!("关闭", "close"))].concat(), w),
            Line::raw(""),
        ];
        if self.rows.is_empty() {
            out.push(Line::styled(t!("还没有分组", "no groups yet"), kit::dim()));
        }
        // 高亮尽量在中间
        let from = self.sel.saturating_sub(SHOWN / 2).min(n.saturating_sub(SHOWN));
        let many = self.rows.iter().any(|r| r.profile != self.rows[0].profile);
        for (i, r) in self.rows.iter().enumerate().skip(from).take(SHOWN) {
            // 不止一个 profile:每个 profile 的头一行上面写它的名字
            if many && (i == from || self.rows[i - 1].profile != r.profile) {
                out.push(Line::styled(format!("profile {}", r.profile), kit::dim()));
            }
            out.push(kit::picked(self.row(r, w), w, i == self.sel));
        }
        out.push(Line::raw(""));
        match self.cur() {
            Some(r) if self.armed => out.extend(self.warning(r).iter().flat_map(|l| wrap(l, theme::warn()))),
            _ => out.extend(wrap(
                t!("删掉分组不动里面的会话:它们挪到上级分组", "deleting a group leaves its sessions alone: they move to the parent group"),
                kit::dim(),
            )),
        }
        out.push(Line::raw(""));
        let keys: &[(&str, &str)] = if self.armed {
            &[("d", t!("删掉", "delete")), ("Esc", t!("不删", "keep"))]
        } else {
            &[("d", t!("删除", "delete")), ("r", t!("重读", "reload"))]
        };
        out.push(kit::keys_line(keys, vec![], w));
        out
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        let armed = std::mem::take(&mut self.armed);
        let n = self.rows.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if armed {
                    fx.say(t!("没删", "not deleted"));
                }
                return Flow::Close;
            }
            KeyCode::Down | KeyCode::Char('j') if n > 0 => self.sel = (self.sel + 1) % n,
            KeyCode::Up | KeyCode::Char('k') if n > 0 => self.sel = (self.sel + n - 1) % n,
            KeyCode::Char('d' | 'x') => match self.cur() {
                None => {}
                Some(_) if !armed => self.armed = true,
                Some(r) => {
                    let force = self.load_of(r).1 > 0;
                    fx.job = Some(Job::Group(Act::Delete { profile: r.profile.clone(), group: r.path.clone(), force }));
                }
            },
            KeyCode::Char('r') => {
                self.reload();
                fx.say(t!("重读了", "reloaded"));
            }
            _ => {}
        }
        Flow::Stay
    }

    fn reload(&mut self) {
        let was = self.cur().map(|r| (r.profile.clone(), r.path.clone()));
        self.all = (self.load)();
        self.rows = self.all.iter().filter(|r| !self.hide.iter().any(|(p, g)| *p == r.profile && *g == r.path)).cloned().collect();
        // 高亮跟着原来那个组;它没了(刚删掉)就留在原来的位置上
        let same = was.and_then(|(p, g)| self.rows.iter().position(|r| r.profile == p && r.path == g));
        self.sel = same.unwrap_or(self.sel).min(self.rows.len().saturating_sub(1));
        self.armed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        /// 假的库:页面每次重读都读它
        static DB: RefCell<Vec<Row>> = const { RefCell::new(vec![]) };
    }

    fn row(profile: &str, path: &str, live: usize, archived: usize) -> Row {
        Row { profile: profile.into(), path: path.into(), live, archived }
    }

    fn set(rows: Vec<Row>) {
        DB.with(|d| *d.borrow_mut() = rows);
    }

    fn fake() -> Vec<Row> {
        DB.with(|d| d.borrow().clone())
    }

    /// 一份分组:有会话的、空的,再加一层子分组和一个藏起来的组
    fn deck() -> Vec<Row> {
        vec![
            row("default", "conductor", 0, 0),
            row("default", "fun", 9, 0),
            row("default", "fun/app", 5, 1),
            row("default", "fun/app/old", 0, 2),
            row("default", "secret", 3, 0),
            row("default", "misc", 0, 0),
            row("default", "ccd", 0, 0),
        ]
    }

    fn page(at: &str) -> Page {
        set(deck());
        Page::new(fake, vec![("default".into(), "secret".into())], ("default", at))
    }

    fn press(p: &mut Page, c: char) -> Fx {
        let mut fx = Fx::default();
        assert_eq!(p.on_key(KeyEvent::from(KeyCode::Char(c)), &mut fx), Flow::Stay);
        fx
    }

    fn text(p: &Page, w: usize) -> Vec<String> {
        p.lines(w).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    fn del(group: &str, force: bool) -> Job {
        Job::Group(Act::Delete { profile: "default".into(), group: group.into(), force })
    }

    #[test]
    fn opens_on_the_group_waiting_for_the_second_d() {
        let mut p = page("ccd");
        let rows = text(&p, 58);
        // 空的组也列着;藏起来的不列
        for want in [" conductor", " fun/app/old", " ccd", "再按 d 删掉 ccd(空的)", "d 删掉 • Esc 不删"] {
            assert!(rows.iter().any(|r| r.contains(want)), "缺「{want}」:{rows:#?}");
        }
        assert!(!rows.iter().any(|r| r.contains("secret")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.starts_with(" fun/app ") && r.ends_with("5 个会话 · 1 个归档")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.starts_with(" fun/app/old") && r.ends_with("2 个归档")), "{rows:#?}");
        // 空的:force = false
        assert_eq!(press(&mut p, 'd').job, Some(del("ccd", false)));
        assert!(!p.armed);
    }

    #[test]
    fn a_group_with_sessions_says_where_they_go_and_needs_force() {
        let warn = |p: &Page| p.warning(p.cur().unwrap());
        let mut p = page("fun/app");
        assert_eq!(warn(&p), ["再按 d 删掉 fun/app", "里面 8 个会话(3 个是归档的)挪到 fun", "1 个子分组一起删"]);
        // 写在页面上,一句一行
        let rows = text(&p, 58);
        assert!(warn(&p).iter().all(|l| rows.contains(l)), "{rows:#?}");
        assert_eq!(press(&mut p, 'd').job, Some(del("fun/app", true)));
        // 顶层的变成没分组的;只有归档的会话也算有会话
        let mut p = page("fun");
        assert_eq!(warn(&p), ["再按 d 删掉 fun", "里面 17 个会话(3 个是归档的)挪到 未分组", "2 个子分组一起删"]);
        assert_eq!(press(&mut p, 'd').job, Some(del("fun", true)));
        let mut p = page("fun/app/old");
        assert_eq!(warn(&p), ["再按 d 删掉 fun/app/old", "里面 2 个会话(2 个是归档的)挪到 fun/app"]);
        assert_eq!(press(&mut p, 'd').job, Some(del("fun/app/old", true)));
    }

    #[test]
    fn moving_away_or_esc_disarms() {
        let mut p = page("ccd");
        assert!(press(&mut p, 'k').job.is_none() && !p.armed);
        // 挪到别的组上:要重新按两次
        assert_eq!(p.cur().unwrap().path, "misc");
        press(&mut p, 'j');
        assert!(press(&mut p, 'd').job.is_none() && p.armed);
        // Esc:不删,页面也关了;没在等的时候不说话
        let mut fx = Fx::default();
        assert_eq!(p.on_key(KeyEvent::from(KeyCode::Esc), &mut fx), Flow::Close);
        assert_eq!(fx.flash.as_deref(), Some("没删"));
        let mut fx = Fx::default();
        assert_eq!(p.on_key(KeyEvent::from(KeyCode::Esc), &mut fx), Flow::Close);
        assert_eq!(fx.flash, None);
    }

    #[test]
    fn a_group_aca_does_not_know_opens_the_page_unarmed() {
        let p = page("-");
        assert_eq!((p.sel, p.armed), (0, false));
        set(vec![]);
        let mut p = Page::new(fake, vec![], ("default", "fun"));
        assert!(text(&p, 58).iter().any(|r| r.contains("还没有分组")));
        assert!(press(&mut p, 'd').job.is_none() && !p.armed);
    }

    #[test]
    fn deleting_a_parent_counts_hidden_subgroups_too() {
        set(vec![row("default", "top", 1, 0), row("default", "top/secret", 2, 0), row("default", "topper", 4, 0)]);
        let mut p = Page::new(fake, vec![("default".into(), "top/secret".into())], ("default", "top"));
        assert!(!text(&p, 58).iter().any(|r| r.contains("top/secret")));
        assert_eq!(p.warning(p.cur().unwrap()), ["再按 d 删掉 top", "里面 3 个会话挪到 未分组", "1 个子分组一起删"]);
        assert_eq!(press(&mut p, 'd').job, Some(del("top", true)));
    }

    #[test]
    fn after_the_delete_the_highlight_stays_in_place() {
        let mut p = page("conductor");
        press(&mut p, 'd');
        set(deck().into_iter().filter(|r| r.path != "conductor").collect());
        p.reload();
        assert_eq!((p.cur().unwrap().path.as_str(), p.armed), ("fun", false));
        // 删的是最后一行:高亮退到新的最后一行
        let mut p = page("ccd");
        set(deck().into_iter().filter(|r| r.path != "ccd").collect());
        p.reload();
        assert_eq!(p.cur().unwrap().path, "misc");
    }

    #[test]
    fn profiles_get_a_heading_only_when_there_are_several() {
        let p = page("fun");
        assert!(!text(&p, 58).iter().any(|r| r.starts_with("profile ")));
        set(vec![row("default", "fun", 1, 0), row("firm", "fun", 0, 0)]);
        let mut p = Page::new(fake, vec![], ("firm", "fun"));
        let rows = text(&p, 58);
        assert!(rows.contains(&"profile default".to_string()) && rows.contains(&"profile firm".to_string()), "{rows:#?}");
        // 同名的组:停在说的那个 profile 的上
        assert_eq!(press(&mut p, 'd').job, Some(Job::Group(Act::Delete { profile: "firm".into(), group: "fun".into(), force: false })));
    }

    #[test]
    fn long_lists_scroll_with_the_highlight() {
        set((0..40).map(|i| row("default", &format!("g{i:02}"), 0, 0)).collect());
        let p = Page::new(fake, vec![], ("default", "g30"));
        let rows = text(&p, 58);
        assert!(rows.iter().any(|r| r.contains("31/40")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains(" g30")) && !rows.iter().any(|r| r.contains(" g00")), "{rows:#?}");
        assert_eq!(rows.iter().filter(|r| r.starts_with(" g")).count(), SHOWN);
        // 手机宽度:一行不超宽
        assert!(text(&p, 40).iter().all(|r| r.width() <= 40));
    }

    #[test]
    fn the_act_knows_what_goes_and_where() {
        let a = Act::Delete { profile: "default".into(), group: "fun/app".into(), force: true };
        assert_eq!(a.dest(), "fun");
        assert!(a.covers("default", "fun/app") && a.covers("default", "fun/app/old"));
        assert!(!a.covers("default", "fun") && !a.covers("default", "fun/apple") && !a.covers("firm", "fun/app"));
        let a = Act::Delete { profile: "default".into(), group: "ccd".into(), force: false };
        assert_eq!(a.dest(), "未分组");
        assert_eq!(a.doing(), "正在删分组 ccd…");
    }

    #[test]
    fn a_rename_reaches_subgroups_only() {
        assert_eq!(renamed("fun", "fun", "play").as_deref(), Some("play"));
        assert_eq!(renamed("fun/app/old", "fun/app", "deck").as_deref(), Some("deck/old"));
        assert_eq!(renamed("fun/apple", "fun/app", "deck"), None);
        assert_eq!(renamed("fun", "fun/app", "deck"), None);
        // 撤销:名字、目录都反过来;删分组撤不了
        let a = Act::Edit { profile: "default".into(), group: "a".into(), to: "b/a".into(), dir: Some(("/x".into(), String::new())) };
        let back = Act::Edit { profile: "default".into(), group: "b/a".into(), to: "a".into(), dir: Some((String::new(), "/x".into())) };
        assert_eq!(a.undo(), Some(back));
        assert_eq!(a.doing(), "正在改分组 a…");
        assert!(!a.covers("default", "a"));
        assert_eq!(Act::Delete { profile: "default".into(), group: "a".into(), force: false }.undo(), None);
    }

    #[test]
    fn spaces_and_the_saved_view_follow_a_renamed_group() {
        let dir = std::env::temp_dir().join(format!("aca-groups-rename-{}", std::process::id()));
        let path = dir.join("spaces");
        crate::state::write_atomic(&path, "group\tdefault\tfun/app\th\ngroup\tdefault\tfun/apple\t2\ngroup\t\t-\t3\n").unwrap();
        let mut sp = crate::spaces::Spaces::open(Some(path.clone()));
        sp.rename_group("default", "fun/app", "deck").unwrap();
        let again = crate::spaces::Spaces::open(Some(path));
        assert_eq!((again.group_of("default", "deck"), again.group_of("default", "fun/app")), (Some('h'), None));
        assert_eq!((again.group_of("default", "fun/apple"), again.group_of("", "-")), (Some('2'), Some('3')));
        let _ = std::fs::remove_dir_all(dir);
        let mut v =
            crate::state::View { group: Some("fun/app/old".into()), folded: vec!["fun".into(), "fun/app".into()], ..Default::default() };
        assert!(v.rename_group("fun/app", "deck"));
        assert_eq!((v.group.as_deref(), v.folded.as_slice()), (Some("deck/old"), &["fun".to_string(), "deck".to_string()][..]));
        assert!(!v.rename_group("nope", "x"));
    }

    #[test]
    fn results_read_like_chinese() {
        assert_eq!(done("lone", 1), "已删分组 lone · 1 个会话挪到 未分组");
        assert_eq!(done("fun/old", 2), "已删分组 fun/old · 2 个会话挪到 fun");
        assert_eq!(done("empty", 0), "已删分组 empty");
    }

    #[test]
    fn the_page_reads_groups_and_counts_from_the_registry() {
        let store = Store::memory();
        let mk = |id: &str, group: &str, archived: u64| aca_core::store::Session {
            id: id.into(),
            title: id.into(),
            tool: "shell".into(),
            group: group.into(),
            dir: "/".into(),
            created: 1,
            archived,
            ..Default::default()
        };
        store.group_create("fun/app", "").unwrap();
        store.group_create("empty", "").unwrap();
        for s in [mk("a", "fun", 0), mk("b", "fun/app", 0), mk("c", "fun/app", 9)] {
            store.insert(&s).unwrap();
        }
        let got: Vec<(String, usize, usize)> = rows_of(&store).into_iter().map(|r| (r.path, r.live, r.archived)).collect();
        assert_eq!(got, [("fun".to_string(), 1, 0), ("fun/app".to_string(), 1, 1), ("empty".to_string(), 0, 0)]);
    }
}
