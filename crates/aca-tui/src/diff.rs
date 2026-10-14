//! `x` 看改动:会话真正干活的目录(见 `workdir`)所在 git 仓库的 diff(像 Claude Squad 的 Diff 页)。
//! 预览顶上的改动统计(`glance`)也在这。
//!
//! 默认看还没提交的(工作区 + 暂存区对 HEAD,没 git add 的新文件也算);`m` 换成连同提交一起、
//! 对比主干的分叉点 —— 这个分支一共改了什么。git 在后台线程里跑(见 ui 的 run 循环),大仓库也不卡界面。
//!
//! 只读:不碰 index(`GIT_OPTIONAL_LOCKS=0`,不跟会话里的 agent 抢 index.lock),不跑外部 diff / textconv。
//! 文件内容里的控制字符换成 �,免得往终端里吐转义序列。

use std::io::Read;
use std::process::{Command, Stdio};

use ratatui::style::{Modifier, Style};

use aca_core::i18n::noun;
use aca_core::{t, tr};

use crate::ansi::{Seg, Styled};
use crate::theme;
use crate::workdir::{self, Where};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// 还没提交的:工作区对 HEAD
    Uncommitted,
    /// 连同提交:工作区对主干的分叉点
    Branch,
}

#[derive(Debug, Default)]
pub struct Diff {
    pub lines: Vec<Styled>,
    /// 每个文件的标题行在 lines 里的下标(n / p 跳)
    pub files: Vec<usize>,
    /// 每行对应 (第几个文件, 新文件里第几行):`e` 在编辑器里打开那里。清单、文件标题指向它第一处改动
    pub at: Vec<Option<(usize, usize)>>,
    /// 按文件:相对仓库根的路径;删掉了的是 None
    pub paths: Vec<Option<String>>,
    /// 仓库根(`l` 开 lazygit、`e` 拼路径);不是仓库时空
    pub top: String,
}

/// 预览顶上那几样:真正干活的目录、它的分支和未提交的改动
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Glance {
    pub work: workdir::Found,
    /// None = 不是 git 仓库
    pub repo: Option<Stat>,
    /// 目录里有什么:分组的预览才读(见 `glance_dir`),会话的是 None
    pub files: Option<Files>,
}

/// 目录里有什么:子目录、文件各自按名字排(不分大小写),点开头的只数个数
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Files {
    pub dirs: Vec<String>,
    pub files: Vec<String>,
    pub hidden: usize,
    /// 读不了:不在了、没权限
    pub err: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stat {
    pub branch: String,
    pub files: usize,
    pub add: usize,
    pub del: usize,
}

/// git 的输出最多读这么多,再多的不要了(比如误改了个几百 MB 的生成文件)
const MAX_BYTES: usize = 8 << 20;
/// 摆出来的最多这么多行
const MAX_LINES: usize = 20_000;
/// 一行太长(压缩过的 js 之类)截掉
const MAX_COLS: usize = 1_000;
/// 没 git add 的新文件:最多摆几个的内容、每个最多几行
const NEW_FILES: usize = 30;
const NEW_FILE_LINES: usize = 300;
/// 空树:还没有提交的仓库拿它当 HEAD
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

fn git(dir: &str, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    c.arg("-C")
        .arg(dir)
        .args(["-c", "core.quotepath=off", "--no-pager"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    c
}

/// 跑一条 git,成功才要输出(去掉结尾换行)
fn run(dir: &str, args: &[&str]) -> Option<String> {
    let out = git(dir, args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// 读到 MAX_BYTES 为止;第二个值 = 是不是截断了
fn run_capped(dir: &str, args: &[&str]) -> Option<(String, bool)> {
    let mut child = git(dir, args).stdout(Stdio::piped()).spawn().ok()?;
    let mut buf = vec![];
    let n = child.stdout.take()?.take(MAX_BYTES as u64 + 1).read_to_end(&mut buf).ok()?;
    let cut = n > MAX_BYTES;
    if cut {
        let _ = child.kill();
        buf.truncate(MAX_BYTES);
        // 截在一行中间:丢掉这半行
        if let Some(p) = buf.iter().rposition(|&b| b == b'\n') {
            buf.truncate(p + 1);
        }
    }
    let ok = child.wait().ok()?.success();
    (ok || cut).then(|| (String::from_utf8_lossy(&buf).into_owned(), cut))
}

/// 主干:origin/HEAD 指向的,没有就 origin/main、origin/master、main、master 里第一个有的
fn trunk(top: &str) -> Option<String> {
    let head = run(top, &["rev-parse", "--abbrev-ref", "origin/HEAD"]).filter(|s| !s.is_empty() && s != "origin/HEAD");
    head.into_iter()
        .chain(["origin/main", "origin/master", "main", "master"].map(String::from))
        .find(|r| run(top, &["rev-parse", "--verify", "-q", &format!("{r}^{{commit}}")]).is_some())
}

fn branch_of(top: &str) -> String {
    run(top, &["symbolic-ref", "--short", "-q", "HEAD"])
        .or_else(|| run(top, &["rev-parse", "--short", "HEAD"]).map(|h| tr!("{h}(没在分支上)", "{h} (detached)")))
        .unwrap_or_default()
}

/// 还没有提交的仓库拿空树当 HEAD
fn head_or_empty(top: &str) -> (bool, String) {
    let has = run(top, &["rev-parse", "--verify", "-q", "HEAD"]).is_some();
    (has, if has { "HEAD".to_string() } else { EMPTY_TREE.to_string() })
}

fn untracked(top: &str) -> Vec<String> {
    run(top, &["ls-files", "--others", "--exclude-standard", "-z"])
        .map(|s| s.split('\0').filter(|p| !p.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

/// 和登记的不是一个目录时,在顶上说一声
fn moved(w: &Where, found: &workdir::Found) -> Option<String> {
    (!found.how.is_empty() && found.dir != w.dir).then(|| tr!("登记在 {}", "registered at {}", crate::create::tilde(&w.dir)))
}

/// 预览顶上的:找目录、数改动(同 `x` 默认看的:未提交的,没 git add 的新文件也算)
pub fn glance(w: &Where) -> Glance {
    let work = workdir::find(w);
    let top = (!work.dir.is_empty() && std::path::Path::new(&work.dir).is_dir())
        .then(|| run(&work.dir, &["rev-parse", "--show-toplevel"]))
        .flatten();
    let Some(top) = top else { return Glance { work, repo: None, files: None } };
    let mut st = Stat { branch: branch_of(&top), ..Default::default() };
    let (_, base) = head_or_empty(&top);
    for l in run(&top, &["diff", "--numstat", "--no-ext-diff", "--no-textconv", "-M", &base]).unwrap_or_default().lines() {
        let mut f = l.split('\t');
        st.files += 1;
        st.add += f.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        st.del += f.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
    }
    for (i, p) in untracked(&top).iter().enumerate() {
        st.files += 1;
        if i < NEW_FILES {
            let t = read_new(&std::path::Path::new(&top).join(p));
            if !t.starts_with('\0') {
                st.add += t.lines().count();
            }
        }
    }
    Glance { work, repo: Some(st), files: None }
}

/// 分组的预览:和会话一样读目录、分支、改动,再列出目录里的东西
pub fn glance_dir(w: &Where) -> Glance {
    let mut g = glance(w);
    g.files = Some(list_dir(&g.work.dir));
    g
}

pub fn list_dir(dir: &str) -> Files {
    let mut out = Files::default();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Files { err: Some(t!("目录不在了", "the directory is gone").into()), ..out };
        }
        Err(e) => return Files { err: Some(tr!("读不了:{e}", "can't read: {e}")), ..out },
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            out.hidden += 1;
            continue;
        }
        // 软链按它指向的算:指向目录的也列在目录里
        let t = e.file_type().ok();
        let is_dir = t.is_some_and(|t| t.is_dir()) || (t.is_some_and(|t| t.is_symlink()) && e.path().is_dir());
        if is_dir { out.dirs.push(name) } else { out.files.push(name) }
    }
    for v in [&mut out.dirs, &mut out.files] {
        v.sort_by_cached_key(|n| n.to_lowercase());
    }
    out
}

/// 找到真正干活的目录,读仓库、跑 diff、摆成带颜色的行
pub fn load(w: &Where, mode: Mode) -> Diff {
    let found = workdir::find(w);
    let dir = found.dir.as_str();
    let moved = moved(w, &found);
    let say =
        |m: &str| notice(&[Some(format!("{} · {m}", crate::create::tilde(dir))), moved.clone()].into_iter().flatten().collect::<Vec<_>>());
    if dir.is_empty() || !std::path::Path::new(dir).is_dir() {
        return say(t!("目录不在了", "the directory is gone"));
    }
    let Some(top) = run(dir, &["rev-parse", "--show-toplevel"]) else {
        return say(t!("不是 git 仓库", "not a git repository"));
    };
    let branch = branch_of(&top);
    let (has_head, mut base) = head_or_empty(&top);
    let mut against = None;
    let mut commits = vec![];
    let mut note = None;
    if mode == Mode::Branch {
        match trunk(&top).filter(|_| has_head) {
            Some(t) => match run(&top, &["merge-base", "HEAD", &t]) {
                Some(mb) => {
                    commits = run(&top, &["log", "--no-color", "--no-decorate", "--format=%h %s", "-n", "30", &format!("{mb}..HEAD")])
                        .map(|s| s.lines().map(String::from).collect())
                        .unwrap_or_default();
                    base = mb;
                    against = Some(t);
                }
                None => note = Some(tr!("和 {t} 没有共同提交", "no common commit with {t}")),
            },
            None => note = Some(t!("没有主干", "no main branch").into()),
        }
    }
    let args = ["diff", "--no-color", "--no-ext-diff", "--no-textconv", "-M", "--src-prefix=a/", "--dst-prefix=b/", base.as_str()];
    let (patch, cut) = run_capped(&top, &args).unwrap_or_default();
    let new_files: Vec<(String, Option<String>)> = untracked(&top)
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let text = (i < NEW_FILES).then(|| read_new(&std::path::Path::new(&top).join(&p)));
            (p, text)
        })
        .collect();
    let head = Head { dir: crate::create::tilde(dir), branch, against, commits, note, cut, moved };
    Diff { top, ..render(&head, &patch, &new_files) }
}

/// 没 git add 的新文件的内容;二进制、读不了的写一句话
fn read_new(p: &std::path::Path) -> String {
    let Ok(f) = std::fs::File::open(p) else { return t!("\0读不了", "\0unreadable").into() };
    let mut buf = vec![];
    if f.take(256 << 10).read_to_end(&mut buf).is_err() {
        return t!("\0读不了", "\0unreadable").into();
    }
    if buf[..buf.len().min(8000)].contains(&0) {
        return t!("\0二进制文件", "\0binary file").into();
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// 摆 diff 用到的仓库信息
#[derive(Default)]
pub struct Head {
    pub dir: String,
    pub branch: String,
    /// Branch 模式对比的主干(origin/main 之类);None = 看的是未提交的
    pub against: Option<String>,
    /// 分叉点之后的提交(一行一个「短 hash 标题」)
    pub commits: Vec<String>,
    /// 想看连同提交的但看不了:为什么
    pub note: Option<String>,
    /// diff 太大,只读了前面一截
    pub cut: bool,
    /// 目录不是登记的那个:登记的是哪
    pub moved: Option<String>,
}

/// 一个文件的改动
#[derive(Debug, Default)]
struct File {
    path: String,
    /// M 改、A 新、D 删、R 改名、? 没 git add
    kind: char,
    add: usize,
    del: usize,
    body: Vec<String>,
    /// 二进制 / 只改了权限之类没有内容行的,写一句
    about: Option<String>,
}

// 增删用状态的绿 / 红、说明用次要色(见 theme.rs):终端的绿和 DIM 在浅底上太淡
fn add_c() -> Style {
    Style::new().fg(theme::pal().green)
}
fn del_c() -> Style {
    Style::new().fg(theme::pal().red)
}
fn dim_c() -> Style {
    theme::minor()
}

fn seg(text: impl Into<String>, style: Style) -> Seg {
    Seg { text: text.into(), style, link: None }
}

fn notice<S: AsRef<str>>(msg: &[S]) -> Diff {
    let lines: Vec<Styled> = msg.iter().map(|m| vec![seg(format!(" {}", m.as_ref()), dim_c())]).collect();
    Diff { at: vec![None; lines.len()], lines, ..Default::default() }
}

/// 摆出来的行,和每行对应文件里的哪儿
#[derive(Default)]
struct Out {
    lines: Vec<Styled>,
    at: Vec<Option<(usize, usize)>>,
}

impl Out {
    fn push(&mut self, l: Styled, at: Option<(usize, usize)>) {
        self.lines.push(l);
        self.at.push(at);
    }
    fn len(&self) -> usize {
        self.lines.len()
    }
}

/// `@@ -a,b +c,d @@` 里的 c
fn hunk_start(line: &str) -> Option<usize> {
    let plus = line.split_whitespace().find(|w| w.starts_with('+'))?;
    plus[1..].split(',').next()?.parse().ok()
}

/// 一个文件的 diff 每行落在新文件的第几行
struct Pos {
    at: Vec<usize>,
    /// 各段第一处加 / 删在第几行,按先后
    changes: Vec<usize>,
}

impl Pos {
    fn first(&self) -> Option<usize> {
        self.changes.first().copied()
    }
}

/// @@ 给起点,加的和没动的往下数,删掉的停在原处;@@ 那行本身指向这一段第一处加 / 删(前面几行是上下文)
fn positions(body: &[String]) -> Pos {
    let mut at = Vec::with_capacity(body.len());
    let mut changes = vec![];
    let mut n = 1;
    let mut hunk: Option<usize> = None;
    for line in body {
        match line.as_bytes().first() {
            Some(b'@') => {
                n = hunk_start(line).unwrap_or(n).max(1);
                hunk = Some(at.len());
                at.push(n);
            }
            Some(c @ (b'+' | b'-')) => {
                if let Some(h) = hunk.take() {
                    at[h] = n;
                    changes.push(n);
                }
                at.push(n);
                if *c == b'+' {
                    n += 1;
                }
            }
            Some(b'\\') => at.push(n.saturating_sub(1).max(1)),
            _ => {
                at.push(n);
                n += 1;
            }
        }
    }
    Pos { at, changes }
}

/// 按 `diff --git` 切成一个个文件
fn parse(patch: &str) -> Vec<File> {
    let mut files: Vec<File> = vec![];
    let mut in_hunk = false;
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // 路径里可能有空格:取最后一个 " b/" 后面的;下面的 +++ / rename to 会再校正
            let path = rest.rfind(" b/").map_or(rest, |i| &rest[i + 3..]).to_string();
            files.push(File { path, kind: 'M', ..Default::default() });
            in_hunk = false;
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if line.starts_with("@@") {
            in_hunk = true;
            f.body.push(line.to_string());
            continue;
        }
        if in_hunk {
            match line.as_bytes().first() {
                Some(b'+') => f.add += 1,
                Some(b'-') => f.del += 1,
                _ => {}
            }
            f.body.push(line.to_string());
            continue;
        }
        if line.starts_with("new file mode") {
            f.kind = 'A';
        } else if line.starts_with("deleted file mode") {
            f.kind = 'D';
        } else if let Some(p) = line.strip_prefix("rename to ") {
            f.kind = 'R';
            f.path = p.to_string();
        } else if let Some(p) = line.strip_prefix("rename from ") {
            f.about = Some(tr!("改名自 {p}", "renamed from {p}"));
        } else if let Some(p) = line.strip_prefix("+++ b/") {
            f.path = p.to_string();
        } else if let Some(p) = line.strip_prefix("--- a/").filter(|_| f.kind == 'D') {
            f.path = p.to_string();
        } else if line.starts_with("Binary files") {
            f.about = Some(t!("二进制文件", "binary file").into());
        } else if let Some(m) = line.strip_prefix("new mode ").filter(|_| f.about.is_none()) {
            f.about = Some(tr!("权限改成 {m}", "mode changed to {m}"));
        }
    }
    files
}

/// 显示用:tab 展开成 4 格一档,回车去掉,别的控制字符换成 �,太长截掉
fn clean(s: &str) -> String {
    let mut out = String::new();
    let mut col = 0;
    for c in s.chars() {
        if col >= MAX_COLS {
            out.push('…');
            break;
        }
        match c {
            '\t' => {
                let n = 4 - col % 4;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\r' => {}
            c if c.is_control() => {
                out.push('�');
                col += 1;
            }
            c => {
                out.push(c);
                col += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            }
        }
    }
    out
}

fn kind_style(k: char) -> Style {
    match k {
        'A' | '?' => add_c(),
        'D' => del_c(),
        'R' => Style::new().fg(theme::pal().hunk),
        _ => theme::warn(),
    }
}

fn counts(f: &File) -> Vec<Seg> {
    let mut v = vec![];
    if f.add > 0 {
        v.push(seg(format!(" +{}", f.add), add_c()));
    }
    if f.del > 0 {
        v.push(seg(format!(" −{}", f.del), del_c()));
    }
    v
}

/// 摆出来:顶上是仓库、分支和文件清单,下面一个文件一段
pub fn render(h: &Head, patch: &str, new_files: &[(String, Option<String>)]) -> Diff {
    let mut files = parse(patch);
    for (path, text) in new_files {
        let mut f = File { path: path.clone(), kind: '?', ..Default::default() };
        match text.as_deref() {
            Some(t) if t.starts_with('\0') => f.about = Some(t[1..].to_string()),
            Some(t) => {
                let lines: Vec<&str> = t.lines().collect();
                f.add = lines.len();
                f.body.push(tr!("@@ 新文件,{} 行 @@", "@@ new file, {} lines @@", lines.len()));
                f.body.extend(lines.iter().take(NEW_FILE_LINES).map(|l| format!("+{l}")));
                if lines.len() > NEW_FILE_LINES {
                    f.body.push(tr!("\\ 还有 {} 行没摆出来", "\\ {} more lines not shown", lines.len() - NEW_FILE_LINES));
                }
            }
            None => f.about = Some(t!("内容没摆出来(新文件太多)", "content not shown (too many new files)").into()),
        }
        files.push(f);
    }
    let bold = Style::new().add_modifier(Modifier::BOLD);
    // body 每行在新文件里第几行;每个文件第一处改动在哪(清单、标题行按 e 去那)
    let pos: Vec<Pos> = files.iter().map(|f| positions(&f.body)).collect();
    let first: Vec<usize> = pos.iter().map(|p| p.first().unwrap_or(1)).collect();
    let mut out = Out::default();
    let mut head = vec![seg(format!(" {}", clean(&h.dir)), Style::new())];
    if !h.branch.is_empty() {
        head.push(seg(format!("  {}", clean(&h.branch)), dim_c()));
    }
    out.push(head, None);
    if let Some(m) = &h.moved {
        out.push(vec![seg(format!(" {}", clean(m)), dim_c())], None);
    }
    let (add, del) = files.iter().fold((0, 0), |(a, d), f| (a + f.add, d + f.del));
    let what = match &h.against {
        Some(t) => tr!("{t} 以来 {} 个提交 + 未提交", "{} commits since {t} + uncommitted", h.commits.len()),
        None => t!("未提交", "uncommitted").into(),
    };
    if files.is_empty() {
        out.push(vec![seg(tr!(" {what}:没有", " {what}: nothing"), bold)], None);
        if h.against.is_none() && h.note.is_none() {
            out.push(vec![seg(t!(" m 连同提交", " m includes commits"), dim_c())], None);
        }
    } else {
        let (k, w) = (files.len(), noun(files.len() as u64, "file", "files"));
        let mut l = vec![seg(tr!(" {what}:{k} 个文件", " {what}: {k} {w}"), bold)];
        l.extend(counts(&File { add, del, ..Default::default() }));
        out.push(l, None);
    }
    if let Some(n) = &h.note {
        out.push(vec![seg(format!(" {n}"), theme::warn())], None);
    }
    if h.cut {
        out.push(vec![seg(t!(" diff 太大,只读了前 8MB", " diff too large, only the first 8MB read"), theme::warn())], None);
    }
    if !h.commits.is_empty() {
        out.push(vec![], None);
        for c in &h.commits {
            let (hash, subject) = c.split_once(' ').unwrap_or((c, ""));
            out.push(vec![seg(format!("  {hash} "), theme::warn()), seg(clean(subject), Style::new())], None);
        }
    }
    // 文件清单
    if !files.is_empty() {
        out.push(vec![], None);
    }
    for (i, f) in files.iter().enumerate() {
        let mut l = vec![seg(format!("  {} ", f.kind), kind_style(f.kind)), seg(clean(&f.path), Style::new())];
        l.extend(counts(f));
        out.push(l, Some((i, first[i])));
    }
    // 一个文件一段
    let mut starts = vec![];
    let mut full = false;
    for (i, f) in files.iter().enumerate() {
        if full {
            break;
        }
        out.push(vec![], None);
        starts.push(out.len());
        let mut l = vec![seg("▌", theme::accent()), seg(format!("{} ", f.kind), kind_style(f.kind).add_modifier(Modifier::BOLD))];
        l.push(seg(clean(&f.path), theme::accent().add_modifier(Modifier::BOLD)));
        l.extend(counts(f));
        out.push(l, Some((i, first[i])));
        if let Some(a) = &f.about {
            out.push(vec![seg(format!("  {a}"), dim_c())], Some((i, first[i])));
        }
        for (line, &n) in f.body.iter().zip(&pos[i].at) {
            if out.len() >= MAX_LINES {
                out.push(vec![seg(t!(" …… 太长了,后面的没摆出来", " …… too long, the rest isn't shown"), theme::warn())], None);
                full = true;
                break;
            }
            let st = match line.as_bytes().first() {
                Some(b'@') => Style::new().fg(theme::pal().hunk),
                Some(b'+') => add_c(),
                Some(b'-') => del_c(),
                Some(b'\\') => dim_c(),
                _ => Style::new(),
            };
            out.push(vec![seg(clean(line), st)], Some((i, n)));
        }
    }
    let paths = files.iter().map(|f| (f.kind != 'D').then(|| f.path.clone())).collect();
    Diff { lines: out.lines, files: starts, at: out.at, paths, top: String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::plain;

    fn text(d: &Diff) -> Vec<String> {
        d.lines.iter().map(plain).collect()
    }

    const PATCH: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,3 @@ fn main() {
 keep
-old
+new
+\tmore
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 3333333..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/old name.md b/new name.md
similarity index 90%
rename from old name.md
rename to new name.md
index 4444444..5555555 100644
--- a/old name.md
+++ b/new name.md
@@ -1 +1 @@
-x
+y
diff --git a/logo.png b/logo.png
index 6666666..7777777 100644
Binary files a/logo.png and b/logo.png differ
";

    #[test]
    fn files_are_listed_up_top_then_one_block_each() {
        let h = Head { dir: "~/p".into(), branch: "main".into(), ..Default::default() };
        let new = [("notes.txt".to_string(), Some("a\nb\n".to_string())), ("pic.bin".to_string(), Some("\0二进制文件".to_string()))];
        let d = render(&h, PATCH, &new);
        let t = text(&d);
        assert_eq!(
            t[..9],
            [
                " ~/p  main",
                " 未提交:6 个文件 +5 −3",
                "",
                "  M src/a.rs +2 −1",
                "  D gone.txt −1",
                "  R new name.md +1 −1",
                "  M logo.png",
                "  ? notes.txt +2",
                "  ? pic.bin",
            ]
        );
        // 每个文件的标题行都记下了位置,n / p 跳这里
        let heads: Vec<&str> = d.files.iter().map(|&i| t[i].as_str()).collect();
        assert_eq!(heads, ["▌M src/a.rs +2 −1", "▌D gone.txt −1", "▌R new name.md +1 −1", "▌M logo.png", "▌? notes.txt +2", "▌? pic.bin"]);
        let a = d.files[0];
        // tab 展开成空格;+ / - 上色
        assert_eq!(t[a + 1..a + 6], ["@@ -1,3 +1,3 @@ fn main() {", " keep", "-old", "+new", "+   more"]);
        assert_eq!(d.lines[a + 3][0].style.fg, Some(theme::pal().red));
        assert_eq!(d.lines[a + 4][0].style.fg, Some(theme::pal().green));
        assert_eq!(t[d.files[2] + 1], "  改名自 old name.md");
        assert_eq!(t[d.files[3] + 1], "  二进制文件");
        assert_eq!(t[d.files[4] + 1..d.files[4] + 4], ["@@ 新文件,2 行 @@", "+a", "+b"]);
        assert_eq!(t[d.files[5] + 1], "  二进制文件");
    }

    #[test]
    fn every_line_knows_where_it_is_in_the_new_file() {
        let d = render(&Head::default(), PATCH, &[("notes.txt".to_string(), Some("a\nb\n".to_string()))]);
        let t = text(&d);
        let at = |s: &str| d.at[t.iter().position(|l| l == s).unwrap()];
        // 清单、标题、@@:第一处加 / 删(不是这段开头的上下文)
        assert_eq!(at("  M src/a.rs +2 −1"), Some((0, 2)));
        assert_eq!(at("▌M src/a.rs +2 −1"), Some((0, 2)));
        assert_eq!(at("@@ -1,3 +1,3 @@ fn main() {"), Some((0, 2)));
        // @@ -1,3 +1,3 @@ 之后: keep=1、-old 停在 2、+new=2、+more=3
        assert_eq!([at(" keep"), at("-old"), at("+new"), at("+   more")], [Some((0, 1)), Some((0, 2)), Some((0, 2)), Some((0, 3))]);
        assert_eq!(at("+b"), Some((4, 2)));
        // 删掉的文件打不开
        assert_eq!(d.paths, [Some("src/a.rs".into()), None, Some("new name.md".into()), Some("logo.png".into()), Some("notes.txt".into())]);
        assert_eq!(d.at.len(), d.lines.len());
    }

    #[test]
    fn nothing_changed_says_so_and_points_at_m() {
        let h = Head { dir: "~/p".into(), branch: "main".into(), ..Default::default() };
        assert_eq!(text(&render(&h, "", &[])), [" ~/p  main", " 未提交:没有", " m 连同提交"]);
        let h = Head { against: Some("origin/main".into()), commits: vec!["abc1234 fix: a thing".into()], ..h };
        let t = text(&render(&h, PATCH, &[]));
        assert_eq!(t[1], " origin/main 以来 1 个提交 + 未提交:4 个文件 +3 −3");
        assert_eq!(t[3], "  abc1234 fix: a thing");
    }

    #[test]
    fn control_characters_never_reach_the_terminal() {
        let patch = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -0,0 +1 @@\n+evil\x1b]52;c;AAAA\x07 end\r\n";
        let d = render(&Head::default(), patch, &[]);
        let last = plain(d.lines.last().unwrap());
        assert_eq!(last, "+evil�]52;c;AAAA� end");
        assert!(d.lines.iter().flatten().all(|s| !s.text.chars().any(char::is_control)));
    }

    #[test]
    fn reads_a_real_repo_without_touching_it() {
        let dir = std::env::temp_dir().join(format!("aca-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();
        let sh = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(d)
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
                .args(args)
                // 不吃本机的 git 配置(签名、钩子之类)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success();
            assert!(ok, "{args:?}");
        };
        // 还没有提交:整个都是新的
        sh(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let w = Where { dir: d.into(), ..Default::default() };
        let t = text(&load(&w, Mode::Uncommitted));
        assert!(t.iter().any(|l| l == "  ? a.txt +1"), "{t:#?}");
        sh(&["add", "."]);
        sh(&["commit", "-qm", "first"]);
        sh(&["checkout", "-qb", "feat"]);
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        sh(&["commit", "-qam", "second"]);
        std::fs::write(dir.join("a.txt"), "three\n").unwrap();
        let index = std::fs::read(dir.join(".git/index")).unwrap();
        // 未提交的:只有 two → three
        let t = text(&load(&w, Mode::Uncommitted));
        assert!(t.contains(&"-two".to_string()) && t.contains(&"+three".to_string()) && !t.contains(&"-one".to_string()), "{t:#?}");
        // 连同提交:从 main 分出来以后的全部
        let t = text(&load(&w, Mode::Branch));
        assert!(t[1].starts_with(" main 以来 1 个提交"), "{t:#?}");
        assert!(t.iter().any(|l| l.ends_with(" second")));
        assert!(t.contains(&"-one".to_string()) && t.contains(&"+three".to_string()), "{t:#?}");
        // 预览顶上的统计:同上面未提交的那份
        let g = glance(&w);
        assert_eq!(g.work.dir, d);
        assert_eq!(g.repo, Some(Stat { branch: "feat".into(), files: 1, add: 1, del: 1 }));
        // 看完 index 原封不动
        assert_eq!(std::fs::read(dir.join(".git/index")).unwrap(), index);
        let root = Where { dir: "/".into(), ..Default::default() };
        assert_eq!(text(&load(&root, Mode::Uncommitted)), [" / · 不是 git 仓库"]);
        assert_eq!(glance(&root).repo, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lists_a_dir_dirs_first_and_counts_the_dot_ones() {
        let dir = std::env::temp_dir().join(format!("aca-ls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["src", "Docs", ".git"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        for f in ["b.txt", "A.md", ".env"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        std::os::unix::fs::symlink(dir.join("src"), dir.join("link")).unwrap();
        let d = dir.to_str().unwrap();
        let f = list_dir(d);
        assert_eq!(
            (f.dirs.clone(), f.files.clone(), f.hidden, f.err.clone()),
            (vec!["Docs".to_string(), "link".into(), "src".into()], vec!["A.md".to_string(), "b.txt".into()], 2, None)
        );
        // 分组的预览:目录、改动之外带着它
        let g = glance_dir(&Where { dir: d.into(), ..Default::default() });
        assert_eq!((g.repo, g.files), (None, Some(f)));
        assert_eq!(glance(&Where { dir: d.into(), ..Default::default() }).files, None);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(list_dir(d).err.as_deref(), Some("目录不在了"));
    }
}
