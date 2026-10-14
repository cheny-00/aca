//! `aca`:命令行。会话后端在 aca-core 里,这里只解析参数、打印结果。

mod cli;
mod complete;
mod help;

fn main() {
    std::process::exit(cli::main(std::env::args().skip(1).collect()));
}
