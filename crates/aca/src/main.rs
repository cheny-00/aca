//! `aca`:命令行。会话后端在 aca-core 里,这里只解析参数、打印结果。

mod cli;
mod complete;
mod help;

fn main() {
    quiet_broken_pipe();
    std::process::exit(cli::main(std::env::args().skip(1).collect()));
}

/// 往管道里打、对面先走了(`aca ls | head -1`、`aca help | less` 没翻完就退):和别的命令行工具一样不声不响地退出(141,
/// 同被 SIGPIPE 带走的),不打 Rust 的 panic。只认 `println!` 这类打不出去的那一种,别的 panic 照旧。
/// 不把 SIGPIPE 改回默认:界面、hook、`aca limits -- 命令` 往子进程写东西时,对面走了要当错误处理,不能被信号带走
fn quiet_broken_pipe() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let p = info.payload();
        let msg = p.downcast_ref::<String>().map(String::as_str).or_else(|| p.downcast_ref::<&str>().copied()).unwrap_or("");
        // EPIPE 在 macOS、Linux 上都是 32
        if msg.starts_with("failed printing to std") && msg.ends_with("(os error 32)") {
            std::process::exit(141);
        }
        prev(info);
    }));
}
