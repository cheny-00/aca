//! New session: the path defaults to the selected session's directory (editable; Tab completes, ↓ / ^F searches with fzf, Enter
//! opens it right there, a missing directory is created), and the group follows the selected session. Tool / account carry over
//! from the selected session; args come from what config.toml sets for that tool and that account (`[tools.X] args`, the account's
//! `args`).
//!
//! Registering and starting is aca-core's `session::create`: the pane runs the agent directly (no shell), ready to enter once made.

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

/// Typed path → absolute path: expand ~, relative paths count from the home directory, drop the trailing /
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

/// The new session being edited after N: the path defaults to the directory of N's target (the selected session), the group follows it
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub text: String,
    /// For picking the group (`group()`): the group follows the typed path
    pub hints: GroupHints,
    /// The selected session's directory (the input box's initial value) and group: while the path is unchanged it stays in this group —
    /// N means "open another one like this session"
    origin: Option<(String, String)>,
    /// Tool / account carried over from the selected session (what `n` picks from the menu replaces them); empty tool = the default
    /// (claude)
    pub tool: String,
    pub account: String,
    /// The selected session's "Claude·account" (the tool name `n` picked); the new-session popup's "tool" shows it, and the new
    /// session uses it
    pub label: String,
    /// The line under the input box: an error message; empty = list completion candidates (menu)
    pub hint: String,
    /// With several Tab-completion candidates: their directory names, the directory they're in (as typed, e.g. "~/code/"), and which one
    /// is current. Cleared on any edit
    pub menu: Vec<String>,
    menu_head: String,
    pub sel: Option<usize>,
    /// Name given with `G`: go into this group regardless of the path (create it if missing)
    pub group_override: Option<String>,
    /// `^N`'s default directory (absolute path): in fzf it and two levels below it come first
    pub base: Option<String>,
    /// ^T switched to a shell: open a shell, cd and start cc / codex yourself (not the carried-over tool / the one picked from the menu)
    pub shell: bool,
}

impl Draft {
    /// `hints` fetches the "path → group" clues (read from the db at run time, faked in unit tests)
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

    /// `^N`: go into the named group, picking from its default directory (the input box holds it; fzf has no query while the path is
    /// unchanged)
    pub fn start_group(&mut self, name: String, base: &str) {
        let base = expand(base);
        self.text = format!("{}/", tilde(&base).trim_end_matches('/'));
        self.origin = Some((self.text.clone(), name.clone()));
        self.group_override = Some(name);
        self.base = Some(base);
        self.edited();
    }

    /// N / n on a group: the path is filled with the group's path dir, unchanged means into this group; tool and account carry over from
    /// the group's session s
    pub fn in_group(s: &Session, group: &str, dir: &str, hints: fn() -> GroupHints) -> Self {
        let mut d = Draft::from(Some(s), hints);
        d.text = tilde(dir);
        if let Some(o) = d.origin.as_mut() {
            *o = (d.text.clone(), group.to_string());
        }
        d
    }

    /// The directory doesn't exist yet (Enter will create it)
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

    /// The group was inferred from the path (no ^N name, path changed): on creation claim it as `aca new -g .` does and record the default
    /// directory
    pub fn by_dir(&self) -> bool {
        let (g, _) = self.group();
        self.group_override.is_none() && !g.is_empty() && !self.origin.as_ref().is_some_and(|(o, _)| expand(o) == expand(&self.text))
    }

    /// New group named with `^N`: once created, record this directory as its default path (existing groups untouched)
    pub fn binds_dir(&self) -> bool {
        self.group_override.is_some() && self.group().1
    }

    /// Whether the new session opens a shell (switched with ^T, or shell picked in n)
    pub fn is_shell(&self) -> bool {
        self.shell || self.tool == "shell"
    }

    /// Which group the new session lands in and whether it's new: path unchanged → the selected session's group; changed → decided by the
    /// path (see `group_for`)
    pub fn group(&self) -> (String, bool) {
        self.group_at(&self.text)
    }

    /// Which group the path would go into if it were dir (every fzf row is marked with it)
    pub fn group_at(&self, dir: &str) -> (String, bool) {
        if let Some(g) = &self.group_override {
            return (g.clone(), !self.hints.groups.iter().any(|(x, _)| x == g));
        }
        match &self.origin {
            Some((o, g)) if expand(o) == expand(dir) => (g.clone(), false),
            _ => group_for(dir, &self.hints),
        }
    }

    /// fzf's query when ↓ opens it: empty if the path was never changed, otherwise the directory names as keywords (~/code/we → "code we")
    pub fn pick_query(&self) -> String {
        if self.origin.as_ref().is_some_and(|(o, _)| expand(o) == expand(&self.text)) {
            return String::new();
        }
        let t = tilde(&expand(&self.text));
        t.trim_start_matches('~').split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
    }

    /// The input changed: the message and the completion candidates are void
    fn edited(&mut self) {
        self.hint.clear();
        self.menu.clear();
        self.sel = None;
    }

    pub fn push(&mut self, c: char) {
        // Typing / = after cycling to a candidate takes it (it already ends in /), then Tab completes inside it
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

    /// → : take the candidate cycled to now
    pub fn accept(&mut self) {
        self.menu.clear();
        self.sel = None;
    }

    /// The directory picked in fzf: ends in /, so typing on creates something under it
    pub fn set_dir(&mut self, dir: &str) {
        self.text = format!("{}/", dir.trim_end_matches('/'));
        self.edited();
    }

    /// Delete the last directory level: ~/code/proj → ~/code/
    pub fn kill_segment(&mut self) {
        let t = self.text.trim_end_matches('/');
        self.text = match t.rfind('/') {
            Some(i) => t[..=i].to_string(),
            None => String::new(),
        };
        self.edited();
    }

    /// Tab-complete directories. Case-insensitive (the iOS keyboard capitalizes the first letter); a unique match completes and adds /;
    /// several complete to the common prefix and are listed on the line above, then each Tab cycles to the next candidate (as zsh does)
    pub fn complete(&mut self) {
        if !self.cycle(1) {
            self.complete_fresh();
        }
    }

    /// Shift-Tab: cycle back to the previous candidate
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

/// Group from path: the same rule as `aca new -g .` (`aca_core::store::group_by_dir`); only groups count, not sessions.
/// Returns (group, the group doesn't exist yet); undecidable (`/`) is empty = no group
pub(crate) fn group_for(text: &str, hints: &GroupHints) -> (String, bool) {
    aca_core::store::group_by_dir(&hints.groups, &aca_core::paths::real(text)).unwrap_or_default()
}

/// Where a candidate directory came from
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Src {
    /// Directories of sessions in the list, registered project directories
    Known,
    /// Frequent directories recorded by zoxide
    Zoxide,
    /// Found next to the ones above
    Near,
}

/// Directory names under base starting with prefix (case-insensitive), sorted; no hidden directories unless prefix starts with .
fn dirs_like(base: &str, prefix: &str) -> Vec<String> {
    let lower = prefix.to_lowercase();
    let mut names: Vec<String> = std::fs::read_dir(base)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir()) // follow symlinks
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

/// Tab in a single-line input box (setting a new group's default directory in the settings page): one match completes and adds /,
/// several complete to the common prefix; None when nothing completes
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

/// Candidates for finding a directory with fzf via ↓ / ^F in `N`. `live` is the directories of sessions in the list (the selected one
/// first), `base` is `^N`'s default directory
pub fn dir_candidates(base: Option<&str>, live: &[String]) -> Vec<(String, Src)> {
    let known = live.iter().cloned().chain(crate::data::project_dirs()).collect();
    let zoxide = Command::new("zoxide")
        .args(["query", "-l"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect())
        .unwrap_or_default();
    base_first(base, candidates_from(known, zoxide, &home()), &home())
}

/// Move `^N`'s default directory and two levels below it to the front (ones already among the usual candidates keep their icon, the
/// rest count as 「附近」 ("nearby"))
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

/// In this order (fzf prefers earlier ones on equal score, and with no input it's the display order), deduplicated, written as ~/…:
/// session / registered project directories → zoxide's frequent directories (it filters out deleted ones itself) → these projects'
/// parent directories themselves and two levels below (new projects usually sit next to old ones). Not the whole home directory: slow,
/// and it would dig into protected directories like ~/Library, so a parent that is the home directory isn't scanned
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

/// Directories within depth levels under root, sorted by name, parents right before their children; skips hidden ones, symlinks
/// and common dependency / build directories
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

/// One directory row in fzf
#[derive(Clone, Debug, PartialEq)]
pub struct DirRow {
    /// ~/…
    pub dir: String,
    /// Icon at the start of the row: ● has sessions ◇ registered ↗ zoxide frequent, space = found nearby
    pub mark: char,
    /// Which group opening here goes into, and whether it's a new group
    pub group: (String, bool),
    /// Sessions already in this directory: (status icon, title)
    pub sessions: Vec<(String, String)>,
}

/// What the fzf opened by ↓ / ^F shows
pub struct DirPick {
    pub rows: Vec<DirRow>,
    /// What's being created ("Claude·main"); empty = the default tool
    pub what: String,
    /// The query made from the path edited in the input box
    pub query: String,
    /// Terminal size: big enough gets the desktop layout (preview on the right), otherwise the phone layout
    pub width: u16,
    pub height: u16,
}

/// What was picked in fzf
#[derive(Debug, PartialEq)]
pub enum Picked {
    /// Enter / double-click: open right here
    Open(String),
    /// →: back into the input box (ending in /); typing a name then = create it under there
    Edit(String),
}

/// Newline inside a field (footer and preview are multi-line, but an fzf candidate is one line)
const NL: char = '\x1f';

impl DirPick {
    fn wide(&self) -> bool {
        self.width >= 100 && self.height >= 24
    }

    /// Four Tab-separated fields per row: absolute path (output when picked, cd'd into by the preview), how it's shown, phone footer,
    /// desktop preview
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
                // Parent paths aren't shortened: fzf searches the shown text, so cut characters couldn't be found. fzf cuts overlong
                // ones at the end and scrolls sideways itself on a match
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
                // The phone footer is always two lines (so the list doesn't jump on wrap)
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
            // → also ends fzf; the first output line is the key pressed: empty = Enter / double-click
            "--expect=right",
            // No --scheme=path: it boosts matches "after a /", so /api-server in a parent path would beat the directory name
            // api-server at the start of the row
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
                // Hide the preview window below 40 columns
                "--preview-window=right,40%,<40(hidden)".into(),
                "--bind=ctrl-/:toggle-preview".into(),
            ]);
        } else {
            // Phone: no border or preview, the screen goes to the list; the two footer lines follow the cursor saying "which group, what's
            // already here", and the input line hugs the keyboard
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

/// Desktop preview: the precomputed part (field 4: directory, which group, existing sessions), plus the git branch / count of
/// uncommitted changes, and what's in the directory
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

/// A session title going into an fzf field: control characters like Tab and newline become spaces
fn one_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// fzf's output (--expect writes the key on the line before) → what was picked
fn parse_pick(out: &str) -> Option<Picked> {
    let (key, dir) = out.split_once('\n')?;
    let dir = tilde(dir.trim());
    if dir.is_empty() {
        return None;
    }
    Some(if key == "right" { Picked::Edit(dir) } else { Picked::Open(dir) })
}

/// Pick a directory with fzf; Esc / Enter with no match → None. fzf draws its own UI on /dev/tty, candidates are fed via stdin;
/// colors etc. follow FZF_DEFAULT_OPTS
pub fn pick_dir(p: &DirPick) -> Result<Option<Picked>, String> {
    let args = p.args();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(fzf(&p.lines(), &args)?.as_deref().and_then(parse_pick))
}

/// Run a single-select fzf once; extra goes after the common args (on duplicate args the later one wins)
pub fn fzf(cands: &[String], extra: &[&str]) -> Result<Option<String>, String> {
    let mut child = Command::new("fzf")
        // The input line is fixed at the bottom (near the phone keyboard), so up = next; Tab does nothing in single-select, so it cycles
        // candidates
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
    // Write from another thread: many candidates would fill the pipe, and fzf may be Esc'd before reading them all
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

/// The session `d` is deleting
#[derive(Clone, Debug, PartialEq)]
pub struct Removal {
    /// The session's key (its id if registered, the session name for a plain tmux session)
    pub name: String,
    pub title: String,
    pub profile: String,
    /// Registered session id; empty = plain tmux session
    pub id: String,
    /// To mention on confirm: it's attached elsewhere (desktop)
    pub worktree: bool,
    pub attached: bool,
    /// Deleting a window opened inside the session / a split pane: (pane id, window is split); only that one pane closes
    pub pane: Option<(String, bool)>,
    /// Plain tmux session: deleting the session's row, but the session has other windows / panes, so only its own pane closes; this is
    /// the pane that becomes the head next ("#2 “zsh”")
    pub next: Option<String>,
    /// Registered session: how many windows / panes opened inside it, which close along with it (mentioned on confirm)
    pub others: usize,
    /// How many child sessions it has (deleting the parent doesn't take the children, mentioned on confirm)
    pub kids: usize,
}

/// How the deletion ended up
#[derive(Clone, Debug, PartialEq)]
pub enum Removed {
    /// Registered session: archived (was = already archived; group = its group), into the Trash, really deleted only after 1 day (see
    /// trash.rs)
    Archived { was: bool, group: String },
    /// Pane / plain tmux session: gone
    Gone,
}

/// Delete a session. A registered one is archived first (tmux session closed, registration kept), goes into the Trash and is really
/// deleted only after 1 day, see trash.rs. Claude / Codex conversation logs are untouched.
/// Other tmux sessions are kill-session'd directly. Waits for the processes in the session to exit cleanly (a few seconds at most),
/// so it runs in the background (`Job::Remove`)
pub fn remove(t: &Target, pane: Option<&str>) -> Result<Removed, String> {
    // tmux closing a pane only sends SIGHUP; processes that ignore it stay running where nobody can find them: reap them first (see
    // park.rs)
    if let Some(pane) = pane {
        return crate::park::close(pane).map(|()| Removed::Gone);
    }
    if t.id.is_empty() {
        // A plain tmux session's key is its session name
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
    /// aca's tool name and account; empty tool = the default (claude)
    pub tool: String,
    pub account: String,
    /// The directory doesn't exist; create it first
    pub create_dir: bool,
    /// New group made with `^N`: after registering, record the directory as the group's default path, so `N` in this directory later goes
    /// into this group (rule 1)
    pub bind: bool,
    /// The group was inferred from the path: let core claim it by directory (`-g .`), recording this directory on a group with no default
    /// directory; when a same-named group records another directory, number onward, and the same directory goes into it next time
    pub by_dir: bool,
}

impl NewReq {
    /// The "tool[:account]" handed to `session::create`
    pub fn spec(&self) -> String {
        crate::tools::spec(if self.tool.is_empty() { DEFAULT_TOOL } else { &self.tool }, &self.account)
    }
}

/// What to start when there's no tool to carry over (N on a plain tmux session)
const DEFAULT_TOOL: &str = "claude";

/// The last reply (copied with `c`): what the hook records at the end of each turn (only the first 4000 chars of a long one)
pub fn last_response(id: &str) -> Option<String> {
    Some(aca_core::runtime::load(id).last).filter(|s| !s.trim().is_empty())
}

pub(crate) fn first_line(b: &[u8]) -> String {
    String::from_utf8_lossy(b).lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string()
}

/// Register + start, returns the session id
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
        // Failing to bind (the directory is already another group's default) doesn't stop the session; N here just won't go into this group
        // automatically later
        let _ = store.group_set_dir(&s.group, &req.dir);
    }
    Ok(s.id)
}

/// Start a registered session (⏎ after stopping it with D): if the pane is still there, start in place, continuing the old conversation
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
        assert_eq!(r.spec(), "claude"); // nothing to carry over: the default tool
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
        assert_eq!(tilde(&format!("{h}x/a")), format!("{h}x/a")); // home followed by more chars doesn't count (/Users/me vs /Users/mex)
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
        // shell picked in n, or opening another one like a shell session: both count
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
                "~/Library/x", // registered (deleted ones dropped)
                "/elsewhere",  // zoxide, duplicates keep only the first
                "~/code",
                "~/code/app",
                "~/code/app/src", // two levels below a parent; no hidden ones, dependency directories or anything deeper
            ]
        );
        let srcs: Vec<Src> = got.iter().map(|(_, s)| *s).collect();
        assert_eq!(srcs, [Src::Known, Src::Known, Src::Known, Src::Zoxide, Src::Near, Src::Near, Src::Near]);
        // A parent that is the home directory (~/top) or ~/Library isn't scanned
        // ^N's default directory and two levels below come first; existing icons unchanged
        let front = base_first(Some(&format!("{h}/code")), got.clone(), &h);
        let dirs: Vec<&str> = front.iter().map(|(d, _)| d.as_str()).collect();
        assert_eq!(dirs, ["~/code", "~/code/app", "~/code/app/src", "~/code/lib", "~/top", "~/Library/x", "/elsewhere"]);
        assert_eq!(front[3].1, Src::Known);
        // Default directory missing: unchanged
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
        assert_eq!(f[0], format!("{home}/code/app")); // both the output and the preview's cd are absolute paths
        assert_eq!(strip_ansi(f[1]), format!("● app{}~/code", " ".repeat(9 + 2))); // the name column is 12 wide (2/5 of 40 columns minus 4)
        // A too-long name widens the row instead of being cut: fzf searches the shown text, cut characters couldn't be found
        assert_eq!(strip_ansi(phone[1].split('\t').nth(1).unwrap()), "◇ very-long-project-name  ~/work/far/away");
        assert_eq!(strip_ansi(phone[2].split('\t').nth(1).unwrap()), "↗ ~");
        // Phone footer, two lines: which group, what's already here
        assert_eq!(f[2], "→ 分组 app\x1f  已有 2 个:修 登录、加测试");
        assert_eq!(phone[1].split('\t').nth(2).unwrap(), "→ 新分组 far/very-long-project-name\x1f");
        // Desktop: one more column for the source; the preview lists sessions and their status
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
        // A phone turned sideways is wide too, but too short
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
        assert_eq!(d.pick_query(), ""); // unchanged
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
        // 1 default path: wins over a same-named group
        assert_eq!(g("/w/my-notes"), ("notes".into(), false));
        assert_eq!(g("/w/b/aca"), ("aca-2".into(), false));
        // A same-named one records another directory: number onward (as aca new -g . does)
        assert_eq!(g("/w/c/aca"), ("aca-3".into(), true));
        // 2 directory name is already a group: top-level same name / the unique ".../name"
        assert_eq!(g("/w/anywhere/fun/"), ("fun".into(), false));
        assert_eq!(g("/w/code/api"), ("work/api".into(), false));
        // 3 parent directory name is a group → nested
        assert_eq!(g("/w/work/newthing"), ("work/newthing".into(), true));
        assert_eq!(g("/w/x/deeper"), ("lab/x/deeper".into(), true));
        // More than one same-named nested group: doesn't count, go on
        assert_eq!(g("/w/code/dup"), ("dup".into(), true));
        // 4 none of these: top-level directory name
        assert_eq!(g("/w/code/brand"), ("brand".into(), true));
    }

    #[test]
    fn unchanged_path_keeps_the_selected_sessions_group() {
        let mut d = draft("~/proj");
        d.origin = Some(("~/proj".into(), "team".into()));
        assert_eq!(d.group(), ("team".into(), false));
        d.text = "~/proj/".into(); // extra /s still count as unchanged
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
        std::fs::write(dir.join("alfile"), "").unwrap(); // files don't count
        let root = dir.to_string_lossy().to_string();
        let mut d = draft(&format!("{root}/al"));
        d.complete();
        assert_eq!(d.text, format!("{root}/alp"));
        assert_eq!((d.menu.clone(), d.sel), (vec!["alpha".to_string(), "alps".to_string()], None));
        // More Tabs cycle through the candidates one by one, wrapping around; Shift-Tab goes back
        d.complete();
        assert_eq!((d.text.clone(), d.sel), (format!("{root}/alpha/"), Some(0)));
        d.complete();
        assert_eq!(d.text, format!("{root}/alps/"));
        d.complete();
        assert_eq!(d.text, format!("{root}/alpha/"));
        d.complete_back();
        assert_eq!(d.text, format!("{root}/alps/"));
        // Typing / takes it (no extra /), and the next Tab completes inside it
        std::fs::create_dir_all(dir.join("alps/inner")).unwrap();
        d.push('/');
        assert_eq!((d.text.clone(), d.menu.len()), (format!("{root}/alps/"), 0));
        d.complete();
        assert_eq!(d.text, format!("{root}/alps/inner/"));
        // Typing on while candidates are listed: candidates void, complete against the new input
        let mut d = draft(&format!("{root}/al"));
        d.complete();
        d.push('h');
        assert!(d.menu.is_empty());
        d.complete();
        assert_eq!(d.text, format!("{root}/alpha/"));
        // → also takes the current one
        let mut d = draft(&format!("{root}/al"));
        d.complete();
        d.complete();
        d.accept();
        assert_eq!((d.text.clone(), d.menu.len()), (format!("{root}/alpha/"), 0));
        let mut d = draft(&format!("{root}/b")); // lowercase still completes to Beta
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
