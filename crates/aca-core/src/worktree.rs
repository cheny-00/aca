//! `aca new --worktree <branch>`: open a git worktree under the repository's own `.worktrees/<branch>` and start the session
//! there. With several agents changing one repository at once, each in its own worktree, a file changed on both sides is a visible
//! conflict when landing, instead of the later write quietly overwriting the earlier one.
//!
//! - Kept inside the repository: Claude sees it as a subdirectory of a folder it already trusts, and doesn't ask about trust
//!   again. `/.worktrees/` is written into `.git/info/exclude` (not a tracked file, so the repository's content doesn't change),
//!   so the main repository's `git status` doesn't show it
//! - The branch is named exactly what you gave, with no prefix: if it doesn't exist it starts from this directory's current
//!   HEAD, if it does it's used as is. A `/` in the branch becomes `-` in the directory name
//! - Created from a subdirectory of the repository, the session starts in the same subdirectory inside the worktree
//! - On `aca rm`, if no other session is in it and it's clean (no changes and no untracked files), it's removed too; the branch
//!   stays, so no commits are lost. One that isn't clean stays, and you're told where it is (no `--force`)
//!
//! A new worktree only has the tracked files: things like `node_modules`, `.venv` and `.env` you install or copy yourself.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths::Res;
use crate::tr;

const DIR: &str = ".worktrees";

fn git(dir: &Path, args: &[&str]) -> Res<String> {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| tr!("无法启动 git:{e}", "can't start git: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// The main repository's root (found from inside a worktree too), git's common directory, and this directory's position relative
/// to its working tree
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

/// Keep `.worktrees/` out of the main repository's `git status`: write it into `info/exclude`, left alone if it's already there
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

/// Open (or find) the worktree for branch `branch`, and return where the session should start
pub fn make(dir: &Path, branch: &str) -> Res<PathBuf> {
    let (main, common, rel) = repo(dir).ok_or_else(|| {
        tr!("{} 不在 git 仓库中:--worktree 只能在仓库中使用", "{} isn't in a git repository: --worktree only works in one", dir.display())
    })?;
    git(dir, &["check-ref-format", "--branch", branch])
        .map_err(|_| tr!("{branch} 不是有效的分支名", "{branch} isn't a valid branch name"))?;
    let path = main.join(DIR).join(branch.replace('/', "-"));
    exclude(&common);
    if path.exists() {
        // Already opened: keep using it, but it has to be on this branch
        if git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]).ok().as_deref() != Some(branch) {
            return Err(tr!(
                "{} 已存在,但不是分支 {branch} 的 worktree",
                "{} already exists but isn't the worktree of branch {branch}",
                path.display()
            ));
        }
    } else {
        let p = path.to_string_lossy();
        let has = git(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok();
        let args: Vec<&str> = if has { vec!["worktree", "add", &p, branch] } else { vec!["worktree", "add", "-b", branch, &p, "HEAD"] };
        git(dir, &args).map_err(|e| tr!("无法创建 worktree:{e}", "can't create the worktree: {e}"))?;
    }
    // Created at the repository root, rel is empty (joining an empty path adds a trailing /)
    let d = path.join(&rel);
    Ok(if !rel.as_os_str().is_empty() && d.is_dir() { d } else { path })
}

/// Which worktree opened by aca `dir` is in: (the worktree's root, the main repository's root)
fn owner(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let (main, _, _) = repo(dir)?;
    let first = dir.strip_prefix(main.join(DIR)).ok()?.components().next()?;
    Some((main.join(DIR).join(first), main))
}

/// A line for people: which worktree and which branch this directory is on
pub fn note(dir: &Path) -> Option<String> {
    let (root, _) = owner(dir)?;
    let branch = git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()?;
    Some(tr!("worktree {}(分支 {branch})", "worktree {} (branch {branch})", root.display()))
}

/// After a session is deleted: if it started in a worktree aca opened and `used` says no other session is there, remove that
/// worktree (the branch stays). Returns a line for people; a session not in a worktree returns None
pub fn release(dir: &Path, used: impl Fn(&Path) -> bool) -> Option<String> {
    let (root, main) = owner(dir)?;
    let branch = git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    if used(&root) {
        return Some(tr!("worktree {} 仍有其他会话在使用,已保留", "worktree {} is still used by another session; kept", root.display()));
    }
    Some(match git(&main, &["worktree", "remove", &root.to_string_lossy()]) {
        Ok(_) => tr!("已删除 worktree {},分支 {branch} 保留", "deleted worktree {}; branch {branch} kept", root.display()),
        Err(_) => tr!(
            "worktree {} 中有未提交的改动或未跟踪的文件,已保留(分支 {branch})",
            "worktree {} has uncommitted changes or untracked files; kept (branch {branch})",
            root.display()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{self, Lang};

    #[test]
    fn a_directory_outside_git_is_told_in_the_interface_language() {
        let dir = std::env::temp_dir().join(format!("aca-worktree-lang-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let zh = make(&dir, "x").unwrap_err();
        let en = i18n::with(Lang::En, || make(&dir, "x").unwrap_err());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(zh.ends_with("不在 git 仓库中:--worktree 只能在仓库中使用"), "{zh}");
        assert!(en.ends_with("isn't in a git repository: --worktree only works in one"), "{en}");
    }
}
