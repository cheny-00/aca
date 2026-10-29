//! `aca`: the command line. The session backend is in aca-core; this only parses arguments and prints results.

mod cli;
mod complete;
mod help;

fn main() {
    quiet_broken_pipe();
    // An argument that isn't UTF-8 (usually the shell swallowed half a CJK character into a variable name: `$pid,`): say which
    // one it is instead of panicking
    let mut args = vec![];
    for a in std::env::args_os().skip(1) {
        match a.into_string() {
            Ok(a) => args.push(a),
            Err(a) => {
                eprintln!("{}", aca_core::tr!("参数不是合法的 UTF-8:{}", "argument is not valid UTF-8: {}", a.to_string_lossy()));
                std::process::exit(1);
            }
        }
    }
    std::process::exit(cli::main(args));
}

/// Printing into a pipe whose reader has left (`aca ls | head -1`, quitting `aca help | less` before the end): exit quietly like
/// other command-line tools do (141, the same as being killed by SIGPIPE) instead of printing a Rust panic. Only the `println!`
/// kind of failing to print counts; other panics go through as before.
/// SIGPIPE isn't reset to the default: when the UI, a hook or `aca limits -- COMMAND` writes to a child process and the other
/// end leaves, that must be handled as an error, not end the process by signal
fn quiet_broken_pipe() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let p = info.payload();
        let msg = p.downcast_ref::<String>().map(String::as_str).or_else(|| p.downcast_ref::<&str>().copied()).unwrap_or("");
        // EPIPE is 32 on both macOS and Linux
        if msg.starts_with("failed printing to std") && msg.ends_with("(os error 32)") {
            std::process::exit(141);
        }
        prev(info);
    }));
}
