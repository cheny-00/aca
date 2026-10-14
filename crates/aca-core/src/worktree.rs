//! `aca new --worktree <分支>`:在仓库自己的 `.worktrees/<分支>` 下开一个 git worktree,会话起在那里。
//! 几个 agent 同时改一个仓库,各在各的 worktree 里:同一个文件被两边改,落地时是看得见的冲突,而不是后写的悄悄盖掉先写的。
//!
//! - 放在仓库里:Claude 认它是已经信任的那个目录的子目录,不再问一遍信不信任。`/.worktrees/` 写进
//!   `.git/info/exclude`(不是被跟踪的文件,不改仓库内容),主仓库的 `git status` 里看不到它
//! - 分支就叫你给的名字,不加前缀:没有就从这个目录现在的 HEAD 开,有就直接用它。分支里的 `/` 在目录名里换成 `-`
//! - 在仓库的子目录里建的,会话起在 worktree 里同一个子目录
//! - `aca rm` 时,别的会话也不在里面、又是干净的(没有改动,也没有没跟踪的文件),一起删掉;分支留着,提交不会丢。
//!   不干净的留着,告诉你在哪(不加 `--force`)
//!
//! 新的 worktree 里只有被跟踪的文件:`node_modules`、`.venv`、`.env` 这类要自己装、自己拷。

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths::Res;

const DIR: &str = ".worktrees";

fn git(dir: &Path, args: &[&str]) -> Res<String> {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| format!("git 起不来:{e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// 主仓库的根(在某个 worktree 里也找回主仓库)、git 的公共目录、这个目录在它那个工作区里的相对位置
fn repo(dir: &Path) -> Option<(PathBuf, PathBuf, PathBuf)> {
    let top = PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"]).ok()?);
    let common = PathBuf::from(git(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).ok()?);
    let main = match common.file_name() {
        Some(n) if n == ".git" => common.parent()?.to_path_buf(),
        _ => top.clone(),
    };
    let rel = dir.strip_prefix(&top).map(Path::to_path_buf).unwrap_or_default();
    Some((main, common, rel))
}

/// `.worktrees/` 不进主仓库的 `git status`:写进 `info/exclude`,已经有了不动
fn exclude(common: &Path) {
    let file = common.join("info").join("exclude");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    if text.lines().any(|l| matches!(l.trim(), "/.worktrees/" | ".worktrees/" | ".worktrees" | "/.worktrees")) {
        return;
    }
    let _ = std::fs::create_dir_all(common.join("info"));
    let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
    let _ = std::fs::write(&file, format!("{text}{sep}/{DIR}/\n"));
}

/// 开好(或找到)分支 `branch` 的 worktree,返回会话该起在哪
pub fn make(dir: &Path, branch: &str) -> Res<PathBuf> {
    let (main, common, rel) = repo(dir).ok_or_else(|| format!("{} 不在 git 仓库里:--worktree 只能在仓库里用", dir.display()))?;
    git(dir, &["check-ref-format", "--branch", branch]).map_err(|_| format!("{branch} 不能当分支名"))?;
    let path = main.join(DIR).join(branch.replace('/', "-"));
    exclude(&common);
    if path.exists() {
        // 已经开过的:接着用,但得是这个分支的
        if git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]).ok().as_deref() != Some(branch) {
            return Err(format!("{} 已经有了,但不是分支 {branch} 的 worktree", path.display()));
        }
    } else {
        let p = path.to_string_lossy();
        let has = git(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok();
        let args: Vec<&str> = if has { vec!["worktree", "add", &p, branch] } else { vec!["worktree", "add", "-b", branch, &p, "HEAD"] };
        git(dir, &args).map_err(|e| format!("开不了 worktree:{e}"))?;
    }
    // 在仓库根上建的,rel 是空的(join 一个空路径会多出结尾的 /)
    let d = path.join(&rel);
    Ok(if !rel.as_os_str().is_empty() && d.is_dir() { d } else { path })
}

/// `dir` 在 aca 开的哪个 worktree 里:(worktree 的根, 主仓库的根)
fn owner(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let (main, _, _) = repo(dir)?;
    let first = dir.strip_prefix(main.join(DIR)).ok()?.components().next()?;
    Some((main.join(DIR).join(first), main))
}

/// 给人看的一句:这个目录在哪个 worktree、哪个分支上
pub fn note(dir: &Path) -> Option<String> {
    let (root, _) = owner(dir)?;
    let branch = git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()?;
    Some(format!("worktree {}(分支 {branch})", root.display()))
}

/// 会话删掉之后:它起在 aca 开的 worktree 里、`used` 说别的会话也不在那里,就把这个 worktree 收掉(分支留着)。
/// 返回给人看的一句;不是 worktree 里的会话返回 None
pub fn release(dir: &Path, used: impl Fn(&Path) -> bool) -> Option<String> {
    let (root, main) = owner(dir)?;
    let branch = git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    if used(&root) {
        return Some(format!("worktree {} 还有别的会话在用,留着", root.display()));
    }
    Some(match git(&main, &["worktree", "remove", &root.to_string_lossy()]) {
        Ok(_) => format!("worktree {} 删了,分支 {branch} 还在", root.display()),
        Err(_) => format!("worktree {} 里有没提交的改动或没跟踪的文件,留着(分支 {branch})", root.display()),
    })
}
