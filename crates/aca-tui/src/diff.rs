//! `x` the diff page: the diff of the git repository holding the directory the session is really working in (see `workdir`),
//! like Claude Squad's Diff page. The change stats atop the preview (`glance`) live here too.
//!
//! By default it shows what's not committed yet (working tree + index vs HEAD, new files never git-added included); `m` switches
//! to including commits, against the fork point from the main branch: everything this branch changed. git runs on a background
//! thread (see the run loop in ui), so a big repository doesn't stall the interface.
//!
//! Read-only: never touches the index (`GIT_OPTIONAL_LOCKS=0`, so it doesn't fight the session's agent for index.lock), runs no
//! external diff / textconv. Control characters in file contents become �, so no escape sequences get spat into the terminal.

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
    /// Not committed yet: working tree vs HEAD
    Uncommitted,
    /// Including commits: working tree vs the fork point from the main branch
    Branch,
}

#[derive(Debug, Default)]
pub struct Diff {
    pub lines: Vec<Styled>,
    /// Index in lines of each file's heading line (n / p jump there)
    pub files: Vec<usize>,
    /// Per line: (which file, which line in the new file); `e` opens that spot in the editor. The file list and file headings
    /// point at the file's first change
    pub at: Vec<Option<(usize, usize)>>,
    /// Per file: path relative to the repository root; None for deleted ones
    pub paths: Vec<Option<String>>,
    /// Repository root (`l` opens lazygit, `e` builds paths on it); empty when not a repository
    pub top: String,
}

/// What sits atop the preview: the directory really being worked in, its branch and uncommitted changes
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Glance {
    pub work: workdir::Found,
    /// None = not a git repository
    pub repo: Option<Stat>,
    /// What's in the directory: read only for a group's preview (see `glance_dir`); None for a session's
    pub files: Option<Files>,
}

/// What's in a directory: subdirectories and files, each sorted by name (case-insensitive); dot entries are only counted
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Files {
    pub dirs: Vec<String>,
    pub files: Vec<String>,
    pub hidden: usize,
    /// Couldn't read it: gone, no permission
    pub err: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stat {
    pub branch: String,
    pub files: usize,
    pub add: usize,
    pub del: usize,
}

/// Read at most this much git output and drop the rest (e.g. a generated file of a few hundred MB changed by mistake)
const MAX_BYTES: usize = 8 << 20;
/// Show at most this many lines
const MAX_LINES: usize = 20_000;
/// Cut lines that are too long (minified js and the like)
const MAX_COLS: usize = 1_000;
/// New files never git-added: show the contents of at most this many, at most this many lines each
const NEW_FILES: usize = 30;
const NEW_FILE_LINES: usize = 300;
/// The empty tree: a repository with no commits yet uses it as HEAD
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

/// Run one git command; take the output only on success (trailing newline trimmed)
fn run(dir: &str, args: &[&str]) -> Option<String> {
    let out = git(dir, args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Read up to MAX_BYTES; second value = whether it was cut
fn run_capped(dir: &str, args: &[&str]) -> Option<(String, bool)> {
    let mut child = git(dir, args).stdout(Stdio::piped()).spawn().ok()?;
    let mut buf = vec![];
    let n = child.stdout.take()?.take(MAX_BYTES as u64 + 1).read_to_end(&mut buf).ok()?;
    let cut = n > MAX_BYTES;
    if cut {
        let _ = child.kill();
        buf.truncate(MAX_BYTES);
        // Cut mid-line: drop the partial line
        if let Some(p) = buf.iter().rposition(|&b| b == b'\n') {
            buf.truncate(p + 1);
        }
    }
    let ok = child.wait().ok()?.success();
    (ok || cut).then(|| (String::from_utf8_lossy(&buf).into_owned(), cut))
}

/// The main branch: what origin/HEAD points to, else the first that exists of origin/main, origin/master, main, master
fn trunk(top: &str) -> Option<String> {
    let head = run(top, &["rev-parse", "--abbrev-ref", "origin/HEAD"]).filter(|s| !s.is_empty() && s != "origin/HEAD");
    head.into_iter()
        .chain(["origin/main", "origin/master", "main", "master"].map(String::from))
        .find(|r| run(top, &["rev-parse", "--verify", "-q", &format!("{r}^{{commit}}")]).is_some())
}

fn branch_of(top: &str) -> String {
    run(top, &["symbolic-ref", "--short", "-q", "HEAD"])
        .or_else(|| run(top, &["rev-parse", "--short", "HEAD"]).map(|h| tr!("{h}(不在分支上)", "{h} (detached)")))
        .unwrap_or_default()
}

/// A repository with no commits yet uses the empty tree as HEAD
fn head_or_empty(top: &str) -> (bool, String) {
    let has = run(top, &["rev-parse", "--verify", "-q", "HEAD"]).is_some();
    (has, if has { "HEAD".to_string() } else { EMPTY_TREE.to_string() })
}

fn untracked(top: &str) -> Vec<String> {
    run(top, &["ls-files", "--others", "--exclude-standard", "-z"])
        .map(|s| s.split('\0').filter(|p| !p.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

/// When it's not the directory the session was opened in, say so at the top
fn moved(w: &Where, found: &workdir::Found) -> Option<String> {
    (!found.how.is_empty() && found.dir != w.dir).then(|| tr!("会话启动目录:{}", "session opened in {}", crate::create::tilde(&w.dir)))
}

/// For the top of the preview: find the directory, count changes (same as `x` shows by default: uncommitted, new files never
/// git-added included)
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

/// A group's preview: read the directory, branch and changes as for a session, then list what's in the directory
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
            return Files { err: Some(t!("目录已不存在", "the directory is gone").into()), ..out };
        }
        Err(e) => return Files { err: Some(tr!("无法读取:{e}", "can't read: {e}")), ..out },
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            out.hidden += 1;
            continue;
        }
        // A symlink counts as what it points to: one pointing to a directory is listed among the directories
        let t = e.file_type().ok();
        let is_dir = t.is_some_and(|t| t.is_dir()) || (t.is_some_and(|t| t.is_symlink()) && e.path().is_dir());
        if is_dir { out.dirs.push(name) } else { out.files.push(name) }
    }
    for v in [&mut out.dirs, &mut out.files] {
        v.sort_by_cached_key(|n| n.to_lowercase());
    }
    out
}

/// Find the directory really being worked in, read the repository, run diff, lay it out as colored lines
pub fn load(w: &Where, mode: Mode) -> Diff {
    let found = workdir::find(w);
    let dir = found.dir.as_str();
    let moved = moved(w, &found);
    let say =
        |m: &str| notice(&[Some(format!("{} · {m}", crate::create::tilde(dir))), moved.clone()].into_iter().flatten().collect::<Vec<_>>());
    if dir.is_empty() || !std::path::Path::new(dir).is_dir() {
        return say(t!("目录已不存在", "the directory is gone"));
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
            None => note = Some(t!("找不到主干分支", "no main branch").into()),
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

/// Contents of a new file never git-added; for binary or unreadable ones, one sentence
fn read_new(p: &std::path::Path) -> String {
    let Ok(f) = std::fs::File::open(p) else { return t!("\0无法读取", "\0unreadable").into() };
    let mut buf = vec![];
    if f.take(256 << 10).read_to_end(&mut buf).is_err() {
        return t!("\0无法读取", "\0unreadable").into();
    }
    if buf[..buf.len().min(8000)].contains(&0) {
        return t!("\0二进制文件", "\0binary file").into();
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Repository info used to lay out the diff
#[derive(Default)]
pub struct Head {
    pub dir: String,
    pub branch: String,
    /// The main branch Branch mode compares against (origin/main or the like); None = showing uncommitted
    pub against: Option<String>,
    /// Commits after the fork point (one per line, "short-hash subject")
    pub commits: Vec<String>,
    /// Wanted to include commits but couldn't: why
    pub note: Option<String>,
    /// The diff was too big, only the first part was read
    pub cut: bool,
    /// The directory isn't the one the session was opened in: where that was
    pub moved: Option<String>,
}

/// One file's changes
#[derive(Debug, Default)]
struct File {
    path: String,
    /// M modified, A new, D deleted, R renamed, ? never git-added
    kind: char,
    add: usize,
    del: usize,
    body: Vec<String>,
    /// For ones with no content lines (binary, only the mode changed and the like), one sentence
    about: Option<String>,
}

// Additions / deletions use the status green / red, notes the minor color (see theme.rs): the terminal's green and DIM are
// too faint on a light background
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

/// The lines laid out, and where in the file each one corresponds to
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

/// The c in `@@ -a,b +c,d @@`
fn hunk_start(line: &str) -> Option<usize> {
    let plus = line.split_whitespace().find(|w| w.starts_with('+'))?;
    plus[1..].split(',').next()?.parse().ok()
}

/// Which line of the new file each line of one file's diff lands on
struct Pos {
    at: Vec<usize>,
    /// The line of each hunk's first addition / deletion, in order
    changes: Vec<usize>,
}

impl Pos {
    fn first(&self) -> Option<usize> {
        self.changes.first().copied()
    }
}

/// @@ gives the start; added and unchanged lines count down, deleted ones stay put; the @@ line itself points at the hunk's
/// first addition / deletion (the lines before it are context)
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

/// Split into files at `diff --git`
fn parse(patch: &str) -> Vec<File> {
    let mut files: Vec<File> = vec![];
    let mut in_hunk = false;
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // The path may contain spaces: take what follows the last " b/"; the +++ / rename to lines below correct it
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
            f.about = Some(tr!("重命名自 {p}", "renamed from {p}"));
        } else if let Some(p) = line.strip_prefix("+++ b/") {
            f.path = p.to_string();
        } else if let Some(p) = line.strip_prefix("--- a/").filter(|_| f.kind == 'D') {
            f.path = p.to_string();
        } else if line.starts_with("Binary files") {
            f.about = Some(t!("二进制文件", "binary file").into());
        } else if let Some(m) = line.strip_prefix("new mode ").filter(|_| f.about.is_none()) {
            f.about = Some(tr!("权限改为 {m}", "mode changed to {m}"));
        }
    }
    files
}

/// For display: tabs expand to 4-column stops, carriage returns go, other control characters become �, too long gets cut
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

/// Lay it out: repository, branch and the file list on top, then one block per file
pub fn render(h: &Head, patch: &str, new_files: &[(String, Option<String>)]) -> Diff {
    let mut files = parse(patch);
    for (path, text) in new_files {
        let mut f = File { path: path.clone(), kind: '?', ..Default::default() };
        match text.as_deref() {
            Some(t) if t.starts_with('\0') => f.about = Some(t[1..].to_string()),
            Some(t) => {
                let lines: Vec<&str> = t.lines().collect();
                f.add = lines.len();
                let (n, w) = (lines.len(), noun(lines.len() as u64, "line", "lines"));
                f.body.push(tr!("@@ 新文件,{n} 行 @@", "@@ new file, {n} {w} @@"));
                f.body.extend(lines.iter().take(NEW_FILE_LINES).map(|l| format!("+{l}")));
                if lines.len() > NEW_FILE_LINES {
                    let (n, w) = (lines.len() - NEW_FILE_LINES, noun((lines.len() - NEW_FILE_LINES) as u64, "line", "lines"));
                    f.body.push(tr!("\\ 还有 {n} 行未显示", "\\ {n} more {w} not shown"));
                }
            }
            None => f.about = Some(t!("内容未显示(新文件过多)", "content not shown (too many new files)").into()),
        }
        files.push(f);
    }
    let bold = Style::new().add_modifier(Modifier::BOLD);
    // Which line in the new file each body line is; where each file's first change is (e on the file list or a heading goes there)
    let pos: Vec<Pos> = files.iter().map(|f| positions(&f.body)).collect();
    let first: Vec<usize> = pos.iter().map(|p| p.first().unwrap_or(1)).collect();
    let mut out = Out::default();
    // Uncommitted / including commits and the file count are in the title bar; here only the total lines added / removed
    let mut head = vec![seg(format!(" {}", clean(&h.dir)), Style::new())];
    if !h.branch.is_empty() {
        head.push(seg(format!("  {}", clean(&h.branch)), dim_c()));
    }
    let (add, del) = files.iter().fold((0, 0), |(a, d), f| (a + f.add, d + f.del));
    head.extend(counts(&File { add, del, ..Default::default() }));
    out.push(head, None);
    if let Some(m) = &h.moved {
        out.push(vec![seg(format!(" {}", clean(m)), dim_c())], None);
    }
    let mut what = vec![];
    if let Some(t) = &h.against {
        what.push(tr!("{t} 以来 {} 个提交", "{} commits since {t}", h.commits.len()));
    }
    if files.is_empty() {
        what.push(t!("没有改动", "no changes").into());
    }
    if !what.is_empty() {
        out.push(vec![seg(format!(" {}", what.join(" · ")), bold)], None);
    }
    if let Some(n) = &h.note {
        out.push(vec![seg(format!(" {n}"), theme::warn())], None);
    }
    if h.cut {
        out.push(vec![seg(t!(" diff 过大,只读取了前 8MB", " diff too large, only the first 8MB read"), theme::warn())], None);
    }
    if !h.commits.is_empty() {
        out.push(vec![], None);
        for c in &h.commits {
            let (hash, subject) = c.split_once(' ').unwrap_or((c, ""));
            out.push(vec![seg(format!("  {hash} "), theme::warn()), seg(clean(subject), Style::new())], None);
        }
    }
    // File list
    if !files.is_empty() {
        out.push(vec![], None);
    }
    for (i, f) in files.iter().enumerate() {
        let mut l = vec![seg(format!("  {} ", f.kind), kind_style(f.kind)), seg(clean(&f.path), Style::new())];
        l.extend(counts(f));
        out.push(l, Some((i, first[i])));
    }
    // One block per file
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
                out.push(vec![seg(t!(" …… 内容过长,其余部分未显示", " …… too long, the rest isn't shown"), theme::warn())], None);
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
            t[..8],
            [
                " ~/p  main +5 −3",
                "",
                "  M src/a.rs +2 −1",
                "  D gone.txt −1",
                "  R new name.md +1 −1",
                "  M logo.png",
                "  ? notes.txt +2",
                "  ? pic.bin",
            ]
        );
        // Every file's heading line has its position recorded; n / p jump here
        let heads: Vec<&str> = d.files.iter().map(|&i| t[i].as_str()).collect();
        assert_eq!(heads, ["▌M src/a.rs +2 −1", "▌D gone.txt −1", "▌R new name.md +1 −1", "▌M logo.png", "▌? notes.txt +2", "▌? pic.bin"]);
        let a = d.files[0];
        // Tabs expand to spaces; + / - are colored
        assert_eq!(t[a + 1..a + 6], ["@@ -1,3 +1,3 @@ fn main() {", " keep", "-old", "+new", "+   more"]);
        assert_eq!(d.lines[a + 3][0].style.fg, Some(theme::pal().red));
        assert_eq!(d.lines[a + 4][0].style.fg, Some(theme::pal().green));
        assert_eq!(t[d.files[2] + 1], "  重命名自 old name.md");
        assert_eq!(t[d.files[3] + 1], "  二进制文件");
        assert_eq!(t[d.files[4] + 1..d.files[4] + 4], ["@@ 新文件,2 行 @@", "+a", "+b"]);
        assert_eq!(t[d.files[5] + 1], "  二进制文件");
    }

    #[test]
    fn every_line_knows_where_it_is_in_the_new_file() {
        let d = render(&Head::default(), PATCH, &[("notes.txt".to_string(), Some("a\nb\n".to_string()))]);
        let t = text(&d);
        let at = |s: &str| d.at[t.iter().position(|l| l == s).unwrap()];
        // File list, heading, @@: the first addition / deletion (not the context at the hunk's start)
        assert_eq!(at("  M src/a.rs +2 −1"), Some((0, 2)));
        assert_eq!(at("▌M src/a.rs +2 −1"), Some((0, 2)));
        assert_eq!(at("@@ -1,3 +1,3 @@ fn main() {"), Some((0, 2)));
        // After @@ -1,3 +1,3 @@: keep=1, -old stays at 2, +new=2, +more=3
        assert_eq!([at(" keep"), at("-old"), at("+new"), at("+   more")], [Some((0, 1)), Some((0, 2)), Some((0, 2)), Some((0, 3))]);
        assert_eq!(at("+b"), Some((4, 2)));
        // A deleted file can't be opened
        assert_eq!(d.paths, [Some("src/a.rs".into()), None, Some("new name.md".into()), Some("logo.png".into()), Some("notes.txt".into())]);
        assert_eq!(d.at.len(), d.lines.len());
    }

    #[test]
    fn nothing_changed_says_so_once() {
        let h = Head { dir: "~/p".into(), branch: "main".into(), ..Default::default() };
        assert_eq!(text(&render(&h, "", &[])), [" ~/p  main", " 没有改动"]);
        let h = Head { against: Some("origin/main".into()), commits: vec!["abc1234 fix: a thing".into()], ..h };
        let t = text(&render(&h, PATCH, &[]));
        assert_eq!(t[..4], [" ~/p  main +3 −3", " origin/main 以来 1 个提交", "", "  abc1234 fix: a thing"]);
        let t = text(&render(&Head { commits: vec![], moved: Some("会话开在 ~/q".into()), ..h }, "", &[]));
        assert_eq!(t, [" ~/p  main", " 会话开在 ~/q", " origin/main 以来 0 个提交 · 没有改动"]);
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
                // Don't pick up this machine's git config (signing, hooks and the like)
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
        // No commits yet: everything is new
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
        // Uncommitted: only two → three
        let t = text(&load(&w, Mode::Uncommitted));
        assert!(t.contains(&"-two".to_string()) && t.contains(&"+three".to_string()) && !t.contains(&"-one".to_string()), "{t:#?}");
        // Including commits: everything since branching off main
        let t = text(&load(&w, Mode::Branch));
        assert!(t[1].starts_with(" main 以来 1 个提交"), "{t:#?}");
        assert!(t.iter().any(|l| l.ends_with(" second")));
        assert!(t.contains(&"-one".to_string()) && t.contains(&"+three".to_string()), "{t:#?}");
        // The stats atop the preview: same as the uncommitted ones above
        let g = glance(&w);
        assert_eq!(g.work.dir, d);
        assert_eq!(g.repo, Some(Stat { branch: "feat".into(), files: 1, add: 1, del: 1 }));
        // After all that reading the index is untouched
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
        // A group's preview: carries the listing besides the directory and changes
        let g = glance_dir(&Where { dir: d.into(), ..Default::default() });
        assert_eq!((g.repo, g.files), (None, Some(f)));
        assert_eq!(glance(&Where { dir: d.into(), ..Default::default() }).files, None);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(list_dir(d).err.as_deref(), Some("目录已不存在"));
    }
}
