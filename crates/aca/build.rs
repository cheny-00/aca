//! Which commit this was built from: `aca --version` prints `aca 0.1.0 (ceaf172 2026-10-19)`. Over a hundred commits sit under
//! one version number, so 0.1.0 alone can't tell which build is installed. Without git (built from a source tarball) it's just
//! the version number.
//!
//! Reruns only when the commit changes (a new commit, a branch switch): watches HEAD and the branch it points to. In a worktree,
//! HEAD is in the worktree's own directory and the branch in the shared one

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let o = Command::new("git").args(args).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !s.is_empty()).then_some(s)
}

fn main() {
    // When the source tarball is unpacked inside some other git repo (say, one tracking the home directory), looking upward finds
    // that repo's commit: this Cargo.toml has to be tracked by this repo
    if git(&["ls-files", "--full-name", "Cargo.toml"]).is_none() {
        return;
    }
    let Some(hash) = git(&["rev-parse", "--short=7", "HEAD"]) else { return };
    let date = git(&["log", "-1", "--format=%cs", "HEAD"]).unwrap_or_default();
    println!("cargo:rustc-env=ACA_COMMIT={}", format!("{hash} {date}").trim());
    if let Some(dir) = git(&["rev-parse", "--path-format=absolute", "--git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
    }
    let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    // Watch the directory the branch file sits in: when the ref is packed into packed-refs the file is gone, and it reappears on
    // the next commit (watching only the file misses that, and the version stays on the old commit)
    if let (Some(common), Some(branch)) = (common, git(&["symbolic-ref", "-q", "HEAD"])) {
        let dir = Path::new(&branch).parent().map(|p| format!("{common}/{}", p.display()));
        for f in dir.into_iter().chain([format!("{common}/packed-refs")]) {
            if Path::new(&f).exists() {
                println!("cargo:rerun-if-changed={f}");
            }
        }
    }
}
