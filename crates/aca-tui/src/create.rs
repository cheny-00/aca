//! 新建会话:路径默认是选中会话的目录(可改,Tab 补全,↓ / ^F 用 fzf 找、回车就开在那,不存在就建),分组跟着
//! 选中会话。工具 / 账号沿用选中会话的,参数走 config.toml 里那个工具、那个账号写的(`[tools.X] args`、账号的 `args`)。
//!
//! 登记和启动是 aca-core 的 `session::create`:窗格里直接跑的就是 agent(不经过 shell),建好就能进。

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use aca_core::i18n::Tr;
use aca_core::session;
use aca_core::store::Store;
use aca_core::{t, tr};
use unicode_width::UnicodeWidthStr;

use crate::data::{GroupHints, Session, tmux_cmd};
use crate::ops::Target;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn home() -> String {
    env("HOME").unwrap_or_default()
}

/// /Users/me/proj → ~/proj
pub fn tilde(dir: &str) -> String {
    let h = home();
    match dir.strip_prefix(&h) {
        Some(rest) if !h.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => dir.to_string(),
    }
}

/// 输入的路径 → 绝对路径:~ 展开,相对路径算在家目录下,去掉结尾的 /
pub(crate) fn expand(text: &str) -> String {
    let t = text.trim();
    let abs = if t == "~" {
        home()
    } else if let Some(rest) = t.strip_prefix("~/") {
        format!("{}/{rest}", home())
    } else if t.starts_with('/') {
        t.to_string()
    } else {
        format!("{}/{t}", home())
    };
    let trimmed = abs.trim_end_matches('/');
    if trimmed.is_empty() { "/".into() } else { trimmed.to_string() }
}

/// 按了 N 之后正在编辑的新会话:路径默认是 N 的对象(选中会话)的目录,分组跟着它
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub text: String,
    /// 定分组用(`group()`):分组跟着输入的路径走
    pub hints: GroupHints,
    /// 选中会话的目录(输入框的初始值)和分组:路径没改就还用这个组 —— N 就是「照这个会话再开一个」
    origin: Option<(String, String)>,
    /// 沿用选中会话的工具 / 账号(`n` 从菜单选的会换掉),工具空 = 默认的(claude)
    pub tool: String,
    pub account: String,
    /// 选中会话的「Claude·账号」(`n` 选的工具名),新建弹窗的「工具」写的就是它,新会话也会是这个
    pub label: String,
    /// 输入框下面那行:错误提示,空 = 列补全候选(menu)
    pub hint: String,
    /// Tab 补全有多个候选时:候选的目录名、它们所在的目录(输入的样子,如 "~/code/")、现在换到了第几个。
    /// 一改输入就清掉
    pub menu: Vec<String>,
    menu_head: String,
    pub sel: Option<usize>,
    /// `G` 起的名字:不管路径,就进这个组(没有就建)
    pub group_override: Option<String>,
    /// `^N` 的默认目录(绝对路径):fzf 里它和它往下两层排最前
    pub base: Option<String>,
    /// ^T 换成了 shell:开个 shell,自己在里面 cd、起 cc / codex(不用沿用的工具 / 菜单选的)
    pub shell: bool,
}

impl Draft {
    /// `hints` 取「路径 → 分组」的线索(run 时读库,单元测试里给假的)
    pub fn from(s: Option<&Session>, hints: fn() -> GroupHints) -> Self {
        let text = match s {
            Some(s) if !s.dir.is_empty() => tilde(&s.dir),
            _ => "~/".into(),
        };
        let hints = hints();
        let origin = s.filter(|s| !s.dir.is_empty() && !s.profile.is_empty() && s.group != "-").map(|s| (text.clone(), s.group.clone()));
        let tool = s.map(|s| s.tool.clone()).unwrap_or_default();
        let account = s.map(|s| s.account.clone()).unwrap_or_default();
        let label = s.map(|s| s.agent_label()).unwrap_or_default();
        Draft {
            text,
            hints,
            origin,
            tool,
            account,
            label,
            hint: String::new(),
            menu: vec![],
            menu_head: String::new(),
            sel: None,
            group_override: None,
            base: None,
            shell: false,
        }
    }

    /// `^N`:进起好名字的组,从默认目录开始挑(输入框里是它,没改路径时 fzf 不带搜索词)
    pub fn start_group(&mut self, name: String, base: &str) {
        let base = expand(base);
        self.text = format!("{}/", tilde(&base).trim_end_matches('/'));
        self.origin = Some((self.text.clone(), name.clone()));
        self.group_override = Some(name);
        self.base = Some(base);
        self.edited();
    }

    /// 在分组上 N / n:路径填组的路径 dir,没改就进这个组;工具、账号沿用组里的会话 s
    pub fn in_group(s: &Session, group: &str, dir: &str, hints: fn() -> GroupHints) -> Self {
        let mut d = Draft::from(Some(s), hints);
        d.text = tilde(dir);
        if let Some(o) = d.origin.as_mut() {
            *o = (d.text.clone(), group.to_string());
        }
        d
    }

    /// 目录还不存在(回车会新建它)
    pub fn is_new_dir(&self) -> bool {
        !self.text.trim().is_empty() && !std::path::Path::new(&expand(&self.text)).exists()
    }

    pub fn finish(&self) -> Result<NewReq, String> {
        if self.text.trim().is_empty() {
            return Err(t!("路径不能为空", "the path is empty").into());
        }
        let (tool, account) = if self.shell { ("shell".to_string(), String::new()) } else { (self.tool.clone(), self.account.clone()) };
        let by_dir = self.by_dir();
        Ok(NewReq {
            dir: expand(&self.text),
            group: self.group().0,
            tool,
            account,
            create_dir: self.is_new_dir(),
            bind: self.binds_dir(),
            by_dir,
        })
    }

    /// 分组是按路径推出来的(没用 ^N 起名、路径也改过了):建的时候和 `aca new -g .` 一样认领,记上默认目录
    pub fn by_dir(&self) -> bool {
        let (g, _) = self.group();
        self.group_override.is_none() && !g.is_empty() && !self.origin.as_ref().is_some_and(|(o, _)| expand(o) == expand(&self.text))
    }

    /// `^N` 起名的新组:建完把这个目录记成它的默认路径(已有的组不动)
    pub fn binds_dir(&self) -> bool {
        self.group_override.is_some() && self.group().1
    }

    /// 新会话开的是不是 shell(^T 换的,或者 n 里选的 shell)
    pub fn is_shell(&self) -> bool {
        self.shell || self.tool == "shell"
    }

    /// 新会话落在哪个分组、是不是新分组:路径没改 → 选中会话的组;改了 → 按路径定(见 `group_for`)
    pub fn group(&self) -> (String, bool) {
        self.group_at(&self.text)
    }

    /// 路径换成 dir 的话会进哪个组(fzf 里每一行都要标)
    pub fn group_at(&self, dir: &str) -> (String, bool) {
        if let Some(g) = &self.group_override {
            return (g.clone(), !self.hints.groups.iter().any(|(x, _)| x == g));
        }
        match &self.origin {
            Some((o, g)) if expand(o) == expand(dir) => (g.clone(), false),
            _ => group_for(dir, &self.hints),
        }
    }

    /// ↓ 打开 fzf 时的搜索词:路径没改过是空的,改过就拿各级目录名当关键词(~/code/we → "code we")
    pub fn pick_query(&self) -> String {
        if self.origin.as_ref().is_some_and(|(o, _)| expand(o) == expand(&self.text)) {
            return String::new();
        }
        let t = tilde(&expand(&self.text));
        t.trim_start_matches('~').split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
    }

    /// 输入变了:提示和补全候选都作废
    fn edited(&mut self) {
        self.hint.clear();
        self.menu.clear();
        self.sel = None;
    }

    pub fn push(&mut self, c: char) {
        // 换到某个候选后打 / = 就要它(它已经带着 /),接着 Tab 补全它里面的
        let accept = c == '/' && self.sel.is_some() && self.text.ends_with('/');
        if !accept {
            self.text.push(c);
        }
        self.edited();
    }

    pub fn backspace(&mut self) {
        self.text.pop();
        self.edited();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.edited();
    }

    /// → :就要现在换到的这个候选
    pub fn accept(&mut self) {
        self.menu.clear();
        self.sel = None;
    }

    /// fzf 选中的目录:结尾带 /,接着打字就是在它下面新建
    pub fn set_dir(&mut self, dir: &str) {
        self.text = format!("{}/", dir.trim_end_matches('/'));
        self.edited();
    }

    /// 删掉最后一级目录:~/code/proj → ~/code/
    pub fn kill_segment(&mut self) {
        let t = self.text.trim_end_matches('/');
        self.text = match t.rfind('/') {
            Some(i) => t[..=i].to_string(),
            None => String::new(),
        };
        self.edited();
    }

    /// Tab 补全目录。不分大小写(iOS 键盘会自动把首字母大写);唯一匹配补全并加 /;
    /// 多个就补到公共前缀并在上面一行列出,之后每按一次 Tab 换成下一个候选(和 zsh 一样)
    pub fn complete(&mut self) {
        if !self.cycle(1) {
            self.complete_fresh();
        }
    }

    /// Shift-Tab:换回上一个候选
    pub fn complete_back(&mut self) {
        if !self.cycle(-1) {
            self.complete_fresh();
        }
    }

    fn cycle(&mut self, step: isize) -> bool {
        if self.menu.is_empty() {
            return false;
        }
        let n = self.menu.len() as isize;
        let i = match self.sel {
            None if step > 0 => 0,
            None => n - 1,
            Some(i) => (i as isize + step).rem_euclid(n),
        } as usize;
        self.sel = Some(i);
        self.text = format!("{}{}/", self.menu_head, self.menu[i]);
        true
    }

    fn complete_fresh(&mut self) {
        self.edited();
        if self.text == "~" {
            self.text.push('/');
            return;
        }
        let (head, prefix) = match self.text.rfind('/') {
            Some(i) => (self.text[..=i].to_string(), self.text[i + 1..].to_string()),
            None => (String::new(), self.text.clone()),
        };
        let base = if head.is_empty() { home() } else { expand(&head) };
        let names = dirs_like(&base, &prefix);
        match names.as_slice() {
            [] => self.hint = tr!("{} 下没有以 {prefix} 开头的目录", "no directory starting with {prefix}… under {}", tilde(&base)),
            [one] => self.text = format!("{head}{one}/"),
            many => {
                let common = common_prefix(many);
                if common.chars().count() >= prefix.chars().count() {
                    self.text = format!("{head}{common}");
                }
                self.menu = many.to_vec();
                self.menu_head = head;
            }
        }
    }
}

/// 路径定分组:和 `aca new -g .` 是同一份规则(`aca_core::store::group_by_dir`),只看分组,不看会话。
/// 返回 (分组, 现在还没有这个组);定不了(`/`)是空的 = 不分组
pub(crate) fn group_for(text: &str, hints: &GroupHints) -> (String, bool) {
    aca_core::store::group_by_dir(&hints.groups, &aca_core::paths::real(text)).unwrap_or_default()
}

/// 候选目录是怎么来的
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Src {
    /// 列表里会话的目录、登记过的项目目录
    Known,
    /// zoxide 记的常去目录
    Zoxide,
    /// 上面这些旁边扫到的
    Near,
}

/// base 下以 prefix 开头(不分大小写)的目录名,排好序;prefix 不以 . 开头时不要隐藏目录
fn dirs_like(base: &str, prefix: &str) -> Vec<String> {
    let lower = prefix.to_lowercase();
    let mut names: Vec<String> = std::fs::read_dir(base)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir()) // 跟随软链
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.to_lowercase().starts_with(&lower) && (prefix.starts_with('.') || !n.starts_with('.')))
                .collect()
        })
        .unwrap_or_default();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

fn common_prefix(many: &[String]) -> String {
    let first: Vec<char> = many[0].chars().collect();
    let n = many.iter().skip(1).fold(first.len(), |n, m| first.iter().zip(m.chars()).take(n).take_while(|(a, b)| **a == *b).count());
    first[..n].iter().collect()
}

/// 单行输入框里的 Tab(设置页改新分组的默认目录):只有一个就补全并加 /,好几个补到公共前缀;补不动返回 None
pub fn complete_path(text: &str) -> Option<String> {
    if text == "~" {
        return Some("~/".into());
    }
    let (head, prefix) = match text.rfind('/') {
        Some(i) => (&text[..=i], &text[i + 1..]),
        None => ("", text),
    };
    let base = if head.is_empty() { home() } else { expand(head) };
    let names = dirs_like(&base, prefix);
    let out = match names.as_slice() {
        [] => return None,
        [one] => format!("{head}{one}/"),
        many => format!("{head}{}", common_prefix(many)),
    };
    (out != text && out.chars().count() >= text.chars().count()).then_some(out)
}

/// `N` 里按 ↓ / ^F 用 fzf 找目录的候选。`live` 是列表里各会话的目录(选中的在最前),`base` 是 `^N` 的默认目录
pub fn dir_candidates(base: Option<&str>, live: &[String]) -> Vec<(String, Src)> {
    let known = live.iter().cloned().chain(crate::data::project_dirs()).collect();
    let zoxide = Command::new("zoxide")
        .args(["query", "-l"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect())
        .unwrap_or_default();
    base_first(base, candidates_from(known, zoxide, &home()), &home())
}

/// `^N` 的默认目录和它往下两层挪到最前(平时的候选里有的保留原来的图标,没有的算「附近」)
fn base_first(base: Option<&str>, rest: Vec<(String, Src)>, home: &str) -> Vec<(String, Src)> {
    let Some(b) = base.filter(|b| Path::new(b).is_dir()) else { return rest };
    let tilde_in = |d: String| match d.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => d,
    };
    let front: Vec<String> = std::iter::once(b.to_string()).chain(subdirs(b, 2)).map(tilde_in).collect();
    let src = |d: &str| rest.iter().find(|(x, _)| x == d).map_or(Src::Near, |(_, s)| *s);
    let mut out: Vec<(String, Src)> = front.iter().map(|d| (d.clone(), src(d))).collect();
    out.extend(rest.into_iter().filter(|(d, _)| !front.contains(d)));
    out
}

/// 按这个顺序排(fzf 同分时靠前的优先,不输入时就是显示顺序),去重,写成 ~/…:
/// 会话 / 登记过的项目目录 → zoxide 记的常去目录(它自己滤掉了已删的)→ 这些项目的上一级目录本身和往下两层
/// (新项目多半和老项目挨着)。不扫整个家目录:慢,还会钻进 ~/Library 这类受保护的目录,所以上一级就是家目录的不扫
fn candidates_from(known: Vec<String>, zoxide: Vec<String>, home: &str) -> Vec<(String, Src)> {
    let known: Vec<String> = known.into_iter().filter(|d| Path::new(d).is_dir()).collect();
    let mut roots: Vec<String> = vec![];
    for d in &known {
        let Some(r) = Path::new(d).parent().and_then(|p| p.to_str()) else { continue };
        if r.strip_prefix(home).is_some_and(|rest| rest.starts_with('/') && !rest.starts_with("/Library")) && !roots.iter().any(|x| x == r)
        {
            roots.push(r.to_string());
        }
    }
    let scanned = roots.iter().flat_map(|r| std::iter::once(r.clone()).chain(subdirs(r, 2)));
    let mut seen = HashSet::new();
    let tilde_in = |d: String| match d.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => d,
    };
    let tag = |src: Src| move |d: String| (d, src);
    known
        .into_iter()
        .map(tag(Src::Known))
        .chain(zoxide.into_iter().map(tag(Src::Zoxide)))
        .chain(scanned.map(tag(Src::Near)))
        .filter(|(d, _)| seen.insert(d.clone()))
        .map(|(d, src)| (tilde_in(d), src))
        .collect()
}

/// root 下 depth 层以内的目录,按名字排、父目录紧跟着子目录;跳过隐藏的、软链和常见的依赖 / 构建目录
fn subdirs(root: &str, depth: usize) -> Vec<String> {
    const SKIP: &[&str] = &["node_modules", "target", "dist", "build", "__pycache__", "venv"];
    let mut names: Vec<String> = std::fs::read_dir(root)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| !n.starts_with('.') && !SKIP.contains(&n.as_str()))
                .collect()
        })
        .unwrap_or_default();
    names.sort_by_key(|n| n.to_lowercase());
    let mut out = vec![];
    for n in names {
        let p = format!("{root}/{n}");
        let below = if depth > 1 { subdirs(&p, depth - 1) } else { vec![] };
        out.push(p);
        out.extend(below);
    }
    out
}

/// fzf 里的一行目录
#[derive(Clone, Debug, PartialEq)]
pub struct DirRow {
    /// ~/…
    pub dir: String,
    /// 行首的图标:● 有会话 ◇ 登记过 ↗ zoxide 常去 空格 = 旁边扫到的
    pub mark: char,
    /// 开在这会进哪个组、是不是新分组
    pub group: (String, bool),
    /// 这个目录里已有的会话:(状态图标, 标题)
    pub sessions: Vec<(String, String)>,
}

/// ↓ / ^F 打开的 fzf 要显示的东西
pub struct DirPick {
    pub rows: Vec<DirRow>,
    /// 新建的是什么(「Claude·主号」),空 = 默认工具
    pub what: String,
    /// 输入框里改过的路径变成的搜索词
    pub query: String,
    /// 终端大小:够大用桌面布局(右边带预览),不然用手机布局
    pub width: u16,
    pub height: u16,
}

/// fzf 里选了什么
#[derive(Debug, PartialEq)]
pub enum Picked {
    /// 回车 / 双击:就开在这
    Open(String),
    /// →:填回输入框(结尾带 /),接着打名字 = 在它下面新建
    Edit(String),
}

/// 字段里的换行(页脚、预览是多行的,但 fzf 一个候选只能占一行)
const NL: char = '\x1f';

impl DirPick {
    fn wide(&self) -> bool {
        self.width >= 100 && self.height >= 24
    }

    /// 每行四个字段,用 Tab 分开:绝对路径(选中后输出、预览里 cd 进去)、显示的样子、手机页脚、桌面预览
    fn lines(&self) -> Vec<String> {
        let wide = self.wide();
        let cap = if wide { 28 } else { (self.width as usize * 2 / 5).saturating_sub(4).max(8) };
        let leafw = self.rows.iter().map(|r| split_dir(&r.dir).0.width()).max().unwrap_or(0).min(cap);
        let (b, d, green, off) = ("\x1b[1m", "\x1b[2m", "\x1b[32m", "\x1b[0m");
        self.rows
            .iter()
            .map(|r| {
                let (leaf, parent) = split_dir(&r.dir);
                let pad = " ".repeat(leafw.saturating_sub(leaf.width()));
                let mark = if r.mark == '●' { format!("{green}●{off}") } else { r.mark.to_string() };
                // 上级路径不截短:fzf 按显示的文字搜,截掉的字就搜不到了。太长的由 fzf 在行尾截,搜到时它会自己横移
                let tag = if wide {
                    let t = match r.mark {
                        '●' => tr!("会话 {}", "sessions {}", r.sessions.len()),
                        '◇' => t!("项目", "project").into(),
                        '↗' => t!("常去", "frequent").into(),
                        _ => t!("附近", "nearby").into(),
                    };
                    format!("{d}{t}{off}{}  ", " ".repeat(7usize.saturating_sub(t.width())))
                } else {
                    String::new()
                };
                let show = if tag.is_empty() && parent.is_empty() {
                    format!("{mark} {b}{leaf}{off}")
                } else {
                    format!("{mark} {b}{leaf}{off}{pad}  {tag}{d}{parent}{off}")
                };
                let (g, new) = &r.group;
                let into = if *new { tr!("新分组 {g}", "new group {g}") } else { tr!("分组 {g}", "group {g}") };
                let titles: Vec<String> = r.sessions.iter().map(|(_, t)| one_line(t)).collect();
                // 手机页脚固定两行(换行时列表不跳)
                let foot = match titles.len() {
                    0 => tr!("→ {into}{NL}", "→ {into}{NL}"),
                    n => tr!("→ {into}{NL}  已有 {n} 个:{}", "→ {into}{NL}  already {n}: {}", titles.join(t!("、", ", "))),
                };
                let mut prev = vec![r.dir.clone(), String::new(), format!("→ {into}")];
                prev.extend(r.sessions.iter().zip(&titles).map(|((i, _), t)| format!("  {i} {t}")));
                prev.push(String::new());
                let s: String = prev.join(&NL.to_string());
                format!("{}\t{show}\t{foot}\t{s}", expand(&r.dir))
            })
            .collect()
    }

    fn args(&self) -> Vec<String> {
        let what = if self.what.is_empty() { t!("会话", " session").to_string() } else { format!(" {}", self.what) };
        let mut a: Vec<String> = [
            "--ansi",
            "--delimiter=\t",
            "--with-nth=2",
            "--accept-nth=1",
            // → 也结束 fzf,输出第一行是按的哪个键:空 = 回车 / 双击
            "--expect=right",
            // 不用 --scheme=path:它给「/ 后面」的匹配加分,上级路径里的 /api-server 会压过行首的目录名 api-server
            "--highlight-line",
            "--prompt=› ",
            t!("--ghost=搜索目录", "--ghost=search directories"),
            "--with-shell=sh -c",
        ]
        .map(String::from)
        .to_vec();
        a.push(format!("--query={}", self.query));
        if self.wide() {
            a.extend([
                "--style=full:rounded".into(),
                "--margin=1,2".into(),
                tr!("--input-label= 新建{what} ", "--input-label= New{what} "),
                format!("--preview={PREVIEW}"),
                // 预览窗不到 40 列就收起
                "--preview-window=right,40%,<40(hidden)".into(),
                "--bind=ctrl-/:toggle-preview".into(),
            ]);
        } else {
            // 手机:没有边框和预览,屏幕留给列表;页脚两行跟着光标说「进哪个组、这里已有什么」,输入行贴着键盘
            a.extend([
                "--no-scrollbar".into(),
                "--info=inline-right".into(),
                "--border=top".into(),
                tr!("--border-label= 新建{what} ", "--border-label= New{what} "),
                "--border-label-pos=2".into(),
                "--footer-border=line".into(),
                r"--bind=focus:transform-footer(printf '%s' {3} | tr '\037' '\n')".into(),
            ]);
        }
        a
    }
}

/// 桌面预览:预先算好的(字段 4:目录、进哪个组、已有的会话),再加 git 分支 / 未提交的改动数、目录里有什么
const PREVIEW: Tr = Tr::new(
    r#"printf '%s\n' {4} | tr '\037' '\n'; cd {1} 2>/dev/null || exit 0; b=$(git branch --show-current 2>/dev/null); if [ -n "$b" ]; then n=$(git status --porcelain 2>/dev/null | wc -l | tr -d ' '); if [ "$n" = 0 ]; then echo "git  $b"; else echo "git  $b · $n 个未提交"; fi; echo; fi; ls -p | head -40"#,
    r#"printf '%s\n' {4} | tr '\037' '\n'; cd {1} 2>/dev/null || exit 0; b=$(git branch --show-current 2>/dev/null); if [ -n "$b" ]; then n=$(git status --porcelain 2>/dev/null | wc -l | tr -d ' '); if [ "$n" = 0 ]; then echo "git  $b"; else echo "git  $b · $n uncommitted"; fi; echo; fi; ls -p | head -40"#,
);

/// ~/code/app → ("app", "~/code");~ → ("~", "");/x → ("x", "/")
fn split_dir(d: &str) -> (&str, &str) {
    let t = d.trim_end_matches('/');
    match t.rfind('/') {
        Some(0) => (&t[1..], "/"),
        Some(i) => (&t[i + 1..], &t[..i]),
        None => (t, ""),
    }
}

/// 会话标题放进 fzf 的字段:Tab、换行这类控制字符换成空格
fn one_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// fzf 的输出(--expect 在前一行写按的键)→ 选了什么
fn parse_pick(out: &str) -> Option<Picked> {
    let (key, dir) = out.split_once('\n')?;
    let dir = tilde(dir.trim());
    if dir.is_empty() {
        return None;
    }
    Some(if key == "right" { Picked::Edit(dir) } else { Picked::Open(dir) })
}

/// 用 fzf 挑一个目录,Esc / 没有匹配时回车 → None。fzf 自己在 /dev/tty 上画界面,候选从 stdin 喂;
/// 配色等随 FZF_DEFAULT_OPTS
pub fn pick_dir(p: &DirPick) -> Result<Option<Picked>, String> {
    let args = p.args();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(fzf(&p.lines(), &args)?.as_deref().and_then(parse_pick))
}

/// 跑一次单选的 fzf,extra 接在公共参数后面(同名参数后面的算)
pub fn fzf(cands: &[String], extra: &[&str]) -> Result<Option<String>, String> {
    let mut child = Command::new("fzf")
        // 输入行固定在底下(靠近手机键盘),所以往上 = 下一个;单选时 Tab 本来什么都不做,拿来换候选
        .args(["--tiebreak=index", "--no-multi", "--layout=default", "--bind=tab:up,btab:down"])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                t!("未安装 fzf", "fzf isn't installed").to_string()
            } else {
                tr!("无法启动 fzf:{e}", "can't start fzf: {e}")
            }
        })?;
    let mut stdin = child.stdin.take().unwrap();
    let text = cands.join("\n");
    // 另起线程写:候选多了会塞满管道,而 fzf 可能没读完就被 Esc 掉了
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(text.as_bytes());
    });
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = writer.join();
    match out.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string()).filter(|s| !s.is_empty())),
        Some(1 | 130) => Ok(None),
        c => Err(tr!("fzf 出错({})", "fzf failed ({})", c.map_or(t!("被信号终止", "killed by a signal").to_string(), |c| c.to_string()))),
    }
}

/// `d` 要删的会话
#[derive(Clone, Debug, PartialEq)]
pub struct Removal {
    /// 会话的键(登记的是它的 id,普通 tmux 会话是会话名)
    pub name: String,
    pub title: String,
    pub profile: String,
    /// 登记的会话 id,空 = 普通 tmux 会话
    pub id: String,
    /// 确认时要说的:别处(桌面)正连着
    pub worktree: bool,
    pub attached: bool,
    /// 删的是会话里另开的窗口 / 分出来的窗格:(窗格 id, 窗口分了屏),只关这一格
    pub pane: Option<(String, bool)>,
    /// 普通 tmux 会话:删的是会话那行,但会话里还有别的窗口 / 窗格,只关它这一格,这里是接着当头的那格(「#2「zsh」」)
    pub next: Option<String>,
    /// 登记的会话:里面另开的窗口 / 窗格有几个,会跟着一起关(确认时说一声)
    pub others: usize,
    /// 它有几个子会话(删父会话不连带子会话,确认时说一声)
    pub kids: usize,
}

/// 删成了什么样
#[derive(Clone, Debug, PartialEq)]
pub enum Removed {
    /// 登记的会话:归档了(was = 本来就归档着;group = 它在哪个组),进回收站,1 天后才真删(见 trash.rs)
    Archived { was: bool, group: String },
    /// 窗格 / 普通 tmux 会话:没了
    Gone,
}

/// 删会话。登记的先归档(tmux 会话关掉、登记留着),进回收站,1 天后才真删,见 trash.rs。Claude / Codex 的对话记录不动。
/// 别的 tmux 会话直接 kill-session。要等会话里的进程退干净(最长几秒),所以在后台跑(`Job::Remove`)
pub fn remove(t: &Target, pane: Option<&str>) -> Result<Removed, String> {
    // tmux 关窗格只发 SIGHUP,不理它的进程会留下来接着跑、哪儿都找不到了:先收干净(见 park.rs)
    if let Some(pane) = pane {
        return crate::park::close(pane).map(|()| Removed::Gone);
    }
    if t.id.is_empty() {
        // 普通 tmux 会话的键就是它的会话名
        let target = format!("={}", t.name);
        crate::park::close_session(&target);
        let o = tmux_cmd().args(["kill-session", "-t", &target]).output().map_err(|e| e.to_string())?;
        return if o.status.success() { Ok(Removed::Gone) } else { Err(first_line(&o.stderr)) };
    }
    let store = Store::open()?;
    let (was, group) = store.get(&t.id).map(|s| (s.archived > 0, s.group)).unwrap_or_default();
    session::archive(&store, &t.id, true)?;
    Ok(Removed::Archived { was, group })
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewReq {
    pub dir: String,
    pub group: String,
    /// aca 的工具名和账号;工具空 = 默认的(claude)
    pub tool: String,
    pub account: String,
    /// 目录不存在,先建
    pub create_dir: bool,
    /// `^N` 建的新组:登记完把目录记成组的默认路径,以后 `N` 到这个目录就进这个组(规则第 1 条)
    pub bind: bool,
    /// 分组是按路径推出来的:交给核心按目录认领(`-g .`),没记默认目录的组记上这个目录;
    /// 同名的组记着别的目录时往后编号,下次同一个目录还进它
    pub by_dir: bool,
}

impl NewReq {
    /// 交给 `session::create` 的「工具[:账号]」
    pub fn spec(&self) -> String {
        crate::tools::spec(if self.tool.is_empty() { DEFAULT_TOOL } else { &self.tool }, &self.account)
    }
}

/// 没有可沿用的工具时(在普通 tmux 会话上按 N)起哪个
const DEFAULT_TOOL: &str = "claude";

/// 最后一条回复(`c` 复制):hook 在每一轮结束时记下的(很长的只留开头 4000 字)
pub fn last_response(id: &str) -> Option<String> {
    Some(aca_core::runtime::load(id).last).filter(|s| !s.trim().is_empty())
}

pub(crate) fn first_line(b: &[u8]) -> String {
    String::from_utf8_lossy(b).lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string()
}

/// 登记 + 启动,返回会话 id
pub fn create(req: &NewReq) -> Result<String, String> {
    if req.create_dir {
        std::fs::create_dir_all(&req.dir).map_err(|e| tr!("无法创建 {}:{e}", "can't create {}: {e}", tilde(&req.dir)))?;
    }
    let store = Store::open()?;
    let cfg = aca_core::config::load();
    let group = if req.by_dir { session::BY_DIR.to_string() } else { req.group.clone() };
    let n = session::New { tool: req.spec(), dir: req.dir.clone(), group, parent: "-".into(), ..Default::default() };
    let s = session::create(&store, &cfg, n)?;
    if req.bind {
        // 没绑上(这个目录已经是别的组的默认目录)也不耽误进会话,只是以后 N 到这里不会自动进这个组
        let _ = store.group_set_dir(&s.group, &req.dir);
    }
    Ok(s.id)
}

/// 启动登记过的会话(D 停掉后按 ⏎ 的):窗格还在就在原地、接着原来的对话起
pub fn start(id: &str) -> Result<(), String> {
    session::start(&Store::open()?, &aca_core::config::load(), id).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_names_the_tool_and_account_the_way_aca_new_takes_them() {
        let r = NewReq {
            dir: "/p".into(),
            group: "g".into(),
            tool: String::new(),
            account: String::new(),
            create_dir: false,
            bind: false,
            by_dir: false,
        };
        assert_eq!(r.spec(), "claude"); // 没有可沿用的:默认工具
        assert_eq!(NewReq { tool: "codex".into(), account: "team".into(), ..r.clone() }.spec(), "codex:team");
        assert_eq!(NewReq { tool: "shell".into(), ..r }.spec(), "shell");
    }

    #[test]
    fn expand_handles_tilde_relative_and_trailing_slash() {
        let h = home();
        assert_eq!(expand("~"), h);
        assert_eq!(expand("~/a/b/"), format!("{h}/a/b"));
        assert_eq!(expand("proj"), format!("{h}/proj"));
        assert_eq!(expand("/opt/x/"), "/opt/x");
        assert_eq!(expand("/"), "/");
        assert_eq!(tilde(&format!("{h}/a")), "~/a");
        assert_eq!(tilde(&format!("{h}x/a")), format!("{h}x/a")); // 家目录后面紧跟别的字不算(/Users/me 之于 /Users/mex)
    }

    fn draft(text: &str) -> Draft {
        Draft {
            text: text.into(),
            hints: GroupHints::default(),
            origin: None,
            tool: String::new(),
            account: String::new(),
            label: String::new(),
            hint: String::new(),
            menu: vec![],
            menu_head: String::new(),
            sel: None,
            group_override: None,
            base: None,
            shell: false,
        }
    }

    #[test]
    fn ctrl_t_turns_the_new_session_into_a_plain_shell() {
        let mut d = Draft { tool: "claude".into(), account: "work".into(), label: "Claude·work".into(), ..draft("/tmp") };
        assert!(!d.is_shell());
        d.shell = true;
        let r = d.finish().unwrap();
        assert_eq!((r.tool.as_str(), r.account.as_str(), r.group.as_str(), r.spec().as_str()), ("shell", "", "tmp", "shell"));
        // n 里选的 shell、照着 shell 会话再开一个,都算
        assert!(Draft { tool: "shell".into(), ..draft("/tmp") }.is_shell());
    }

    #[test]
    fn candidates_put_known_first_then_zoxide_then_neighbours() {
        let h = std::env::temp_dir().join(format!("aca-cand-{}", std::process::id()));
        let h = h.to_str().unwrap().to_string();
        for d in ["code/app", "code/app/src", "code/app/src/deep", "code/lib", "code/.hidden", "code/node_modules", "top", "Library/x"] {
            std::fs::create_dir_all(format!("{h}/{d}")).unwrap();
        }
        let got = candidates_from(
            vec![format!("{h}/code/lib"), format!("{h}/gone"), format!("{h}/top"), format!("{h}/Library/x")],
            vec![format!("{h}/code/lib"), "/elsewhere".into()],
            &h,
        );
        let dirs: Vec<&str> = got.iter().map(|(d, _)| d.as_str()).collect();
        assert_eq!(
            dirs,
            [
                "~/code/lib",
                "~/top",
                "~/Library/x", // 登记过的(删掉的不要)
                "/elsewhere",  // zoxide,重复的只留前面那个
                "~/code",
                "~/code/app",
                "~/code/app/src", // 上一级往下两层;隐藏的、依赖目录、再深的不要
            ]
        );
        let srcs: Vec<Src> = got.iter().map(|(_, s)| *s).collect();
        assert_eq!(srcs, [Src::Known, Src::Known, Src::Known, Src::Zoxide, Src::Near, Src::Near, Src::Near]);
        // 上一级是家目录(~/top)或 ~/Library 的不扫
        // ^N 的默认目录和往下两层排最前,原来有的图标不变
        let front = base_first(Some(&format!("{h}/code")), got.clone(), &h);
        let dirs: Vec<&str> = front.iter().map(|(d, _)| d.as_str()).collect();
        assert_eq!(dirs, ["~/code", "~/code/app", "~/code/app/src", "~/code/lib", "~/top", "~/Library/x", "/elsewhere"]);
        assert_eq!(front[3].1, Src::Known);
        // 默认目录不在:照旧
        assert_eq!(base_first(Some(&format!("{h}/nope")), got.clone(), &h), got);
        let _ = std::fs::remove_dir_all(&h);
    }

    fn pick(width: u16, height: u16) -> DirPick {
        let row = |dir: &str, mark, g: &str, new, sessions: &[(&str, &str)]| DirRow {
            dir: dir.into(),
            mark,
            group: (g.into(), new),
            sessions: sessions.iter().map(|(i, t)| (i.to_string(), t.to_string())).collect(),
        };
        DirPick {
            rows: vec![
                row("~/code/app", '●', "app", false, &[("!", "修\t登录"), ("▶", "加测试")]),
                row("~/work/far/away/very-long-project-name", '◇', "far/very-long-project-name", true, &[]),
                row("~", '↗', "~", true, &[]),
            ],
            what: "Claude·甲".into(),
            query: String::new(),
            width,
            height,
        }
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut it = s.chars();
        while let Some(c) = it.next() {
            if c == '\x1b' {
                it.by_ref().find(|&c| c == 'm');
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn fzf_rows_show_the_name_first_and_never_cut_what_search_needs() {
        let home = std::env::var("HOME").unwrap();
        let phone = pick(40, 16).lines();
        let f: Vec<&str> = phone[0].split('\t').collect();
        assert_eq!(f.len(), 4, "Tab 只用来分字段(标题里的 Tab 换成了空格)");
        assert_eq!(f[0], format!("{home}/code/app")); // 输出的、预览 cd 的都是绝对路径
        assert_eq!(strip_ansi(f[1]), format!("● app{}~/code", " ".repeat(9 + 2))); // 名字那列 12 宽(40 列的 2/5 减 4)
        // 太长的名字把这一行撑开也不截:fzf 按显示的文字搜,截掉的字就搜不到了
        assert_eq!(strip_ansi(phone[1].split('\t').nth(1).unwrap()), "◇ very-long-project-name  ~/work/far/away");
        assert_eq!(strip_ansi(phone[2].split('\t').nth(1).unwrap()), "↗ ~");
        // 手机页脚两行:进哪个组、这里已有什么
        assert_eq!(f[2], "→ 分组 app\x1f  已有 2 个:修 登录、加测试");
        assert_eq!(phone[1].split('\t').nth(2).unwrap(), "→ 新分组 far/very-long-project-name\x1f");
        // 桌面:多一列来源;预览里列出会话和状态
        let desk = pick(120, 30).lines();
        let f: Vec<&str> = desk[0].split('\t').collect();
        assert_eq!(strip_ansi(f[1]), "● app                     会话 2   ~/code");
        assert_eq!(f[3], "~/code/app\x1f\x1f→ 分组 app\x1f  ! 修 登录\x1f  ▶ 加测试\x1f");
    }

    #[test]
    fn fzf_layout_follows_the_terminal_size() {
        let has = |p: &DirPick, a: &str| p.args().iter().any(|x| x.starts_with(a));
        let phone = pick(40, 16);
        assert!(has(&phone, "--bind=focus:transform-footer") && !has(&phone, "--preview="));
        assert!(phone.args().contains(&"--border-label= 新建 Claude·甲 ".to_string()));
        // 横过来的手机也是宽的,但太矮
        assert!(!has(&pick(110, 14), "--preview="));
        let desk = pick(120, 30);
        assert!(has(&desk, "--preview=") && has(&desk, "--style=full"));
        assert!(desk.args().contains(&"--input-label= 新建 Claude·甲 ".to_string()));
        let mut plain = pick(40, 16);
        plain.what.clear();
        assert!(plain.args().contains(&"--border-label= 新建会话 ".to_string()));
    }

    #[test]
    fn enter_opens_right_arrow_fills_the_box() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(parse_pick(&format!("\n{home}/code/app")), Some(Picked::Open("~/code/app".into())));
        assert_eq!(parse_pick(&format!("right\n{home}/code/app")), Some(Picked::Edit("~/code/app".into())));
        assert_eq!(parse_pick("\n"), None);
        assert_eq!(parse_pick(""), None);
    }

    #[test]
    fn edited_path_becomes_the_fzf_query() {
        let mut d = draft("~/old");
        d.origin = Some(("~/old".into(), "g".into()));
        assert_eq!(d.pick_query(), ""); // 没改过
        d.text = "~/code/we".into();
        assert_eq!(d.pick_query(), "code we");
        d.text = format!("{}/code/", std::env::var("HOME").unwrap());
        assert_eq!(d.pick_query(), "code");
        d.text = "~/".into();
        assert_eq!(d.pick_query(), "");
        assert_eq!(split_dir("/x"), ("x", "/"));
    }

    #[test]
    fn picked_dir_ends_with_slash_for_typing_a_new_one() {
        let mut d = draft("~/old");
        d.hint = "候选".into();
        d.set_dir("~/code/app");
        assert_eq!((d.text.as_str(), d.hint.as_str()), ("~/code/app/", ""));
        d.set_dir("~/x/");
        assert_eq!(d.text, "~/x/");
    }

    #[test]
    fn group_rules_pick_a_group_from_the_directory() {
        let groups = |v: &[(&str, &str)]| GroupHints { groups: v.iter().map(|(g, d)| (g.to_string(), d.to_string())).collect() };
        let h = groups(&[
            ("fun", ""),
            ("work", ""),
            ("work/api", ""),
            ("a/dup", ""),
            ("b/dup", ""),
            ("notes", "/w/my-notes"),
            ("lab/x", ""),
            ("aca", "/w/a/aca"),
            ("aca-2", "/w/b/aca"),
        ]);
        let g = |t: &str| group_for(t, &h);
        // 1 默认路径:比同名的组优先
        assert_eq!(g("/w/my-notes"), ("notes".into(), false));
        assert_eq!(g("/w/b/aca"), ("aca-2".into(), false));
        // 同名的记着别的目录:往后编号(和 aca new -g . 一样)
        assert_eq!(g("/w/c/aca"), ("aca-3".into(), true));
        // 2 目录名已是分组:顶层同名 / 唯一的「…/名字」
        assert_eq!(g("/w/anywhere/fun/"), ("fun".into(), false));
        assert_eq!(g("/w/code/api"), ("work/api".into(), false));
        // 3 上一级目录名是分组 → 嵌套
        assert_eq!(g("/w/work/newthing"), ("work/newthing".into(), true));
        assert_eq!(g("/w/x/deeper"), ("lab/x/deeper".into(), true));
        // 同名的嵌套组不止一个:不算,往下走
        assert_eq!(g("/w/code/dup"), ("dup".into(), true));
        // 4 都不是:顶层目录名
        assert_eq!(g("/w/code/brand"), ("brand".into(), true));
    }

    #[test]
    fn unchanged_path_keeps_the_selected_sessions_group() {
        let mut d = draft("~/proj");
        d.origin = Some(("~/proj".into(), "team".into()));
        assert_eq!(d.group(), ("team".into(), false));
        d.text = "~/proj/".into(); // 多个 / 也算没改
        assert_eq!(d.group(), ("team".into(), false));
        d.text = "~/other".into();
        assert_eq!(d.group(), ("other".into(), true));
    }

    #[test]
    fn kill_segment_drops_one_level() {
        let mut d = draft("~/code/proj");
        d.kill_segment();
        assert_eq!(d.text, "~/code/");
        d.kill_segment();
        assert_eq!(d.text, "~/");
        d.kill_segment();
        assert_eq!(d.text, "");
    }

    #[test]
    fn tab_completes_dirs_case_insensitively() {
        let dir = std::env::temp_dir().join(format!("aca-complete-{}", std::process::id()));
        for d in ["alpha", "alps", "Beta", ".hidden"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(dir.join("alfile"), "").unwrap(); // 文件不算
        let root = dir.to_string_lossy().to_string();
        let mut d = draft(&format!("{root}/al"));
        d.complete();
        assert_eq!(d.text, format!("{root}/alp"));
        assert_eq!((d.menu.clone(), d.sel), (vec!["alpha".to_string(), "alps".to_string()], None));
        // 再按 Tab 挨个换成候选,转一圈回来;Shift-Tab 往回
        d.complete();
        assert_eq!((d.text.clone(), d.sel), (format!("{root}/alpha/"), Some(0)));
        d.complete();
        assert_eq!(d.text, format!("{root}/alps/"));
        d.complete();
        assert_eq!(d.text, format!("{root}/alpha/"));
        d.complete_back();
        assert_eq!(d.text, format!("{root}/alps/"));
        // 打 / 就要它(不多出一个 /),再 Tab 是补它里面的
        std::fs::create_dir_all(dir.join("alps/inner")).unwrap();
        d.push('/');
        assert_eq!((d.text.clone(), d.menu.len()), (format!("{root}/alps/"), 0));
        d.complete();
        assert_eq!(d.text, format!("{root}/alps/inner/"));
        // 列着候选时接着打字:候选作废,按新输入补
        let mut d = draft(&format!("{root}/al"));
        d.complete();
        d.push('h');
        assert!(d.menu.is_empty());
        d.complete();
        assert_eq!(d.text, format!("{root}/alpha/"));
        // → 也是就要现在这个
        let mut d = draft(&format!("{root}/al"));
        d.complete();
        d.complete();
        d.accept();
        assert_eq!((d.text.clone(), d.menu.len()), (format!("{root}/alpha/"), 0));
        let mut d = draft(&format!("{root}/b")); // 小写也能补出 Beta
        d.complete();
        assert_eq!(d.text, format!("{root}/Beta/"));
        let mut d = draft(&format!("{root}/zz"));
        d.complete();
        assert!(d.hint.contains("没有"));
        assert!(d.is_new_dir());
        assert!(d.finish().unwrap().create_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
