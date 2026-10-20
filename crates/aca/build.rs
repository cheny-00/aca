//! 编的是哪一个提交:`aca --version` 打成 `aca 0.1.0 (ceaf172 2026-10-19)`。同一个版本号底下隔了一百多个提交,
//! 光看 0.1.0 分不出装的是哪一份。没有 git(从源码包编的)就只有版本号。
//!
//! 提交变了(新提交、换分支)才重跑:盯着 HEAD 和它指的那个分支。worktree 里 HEAD 在它自己的目录,分支在共用的那个

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let o = Command::new("git").args(args).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !s.is_empty()).then_some(s)
}

fn main() {
    // 源码包解在别的 git 仓库里(比如管家目录的那个)时,往上找到的是人家的提交:这份 Cargo.toml 得是这个仓库管着的
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
    if let (Some(common), Some(branch)) = (common, git(&["symbolic-ref", "-q", "HEAD"])) {
        for f in [format!("{common}/{branch}"), format!("{common}/packed-refs")] {
            if Path::new(&f).exists() {
                println!("cargo:rerun-if-changed={f}");
            }
        }
    }
}
