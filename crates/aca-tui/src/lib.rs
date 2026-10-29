//! aca's interface: one screen lists every session (registered ones, plain tmux ones), shows the highlighted one's live screen,
//! ⏎ goes into it.
//!
//! All data comes from aca-core (the registry, the state hooks write, the panes actually in tmux), and operations on sessions
//! call its functions too; this crate only handles how to list, how to draw, what keys do, and a few small states of its own
//! (pins, notes, seen, workspaces; see state.rs).

mod accounts;
mod acts;
mod acctpage;
mod ansi;
mod bar;
mod bill;
mod browser;
mod caps;
mod clicked;
mod clip;
mod config;
mod copies;
mod create;
mod data;
mod diff;
mod exitlog;
mod follow;
mod groups;
mod gwpage;
mod handoff;
mod help;
mod hl;
mod ime;
mod limits;
mod mcp;
mod notes;
mod ops;
mod overlay;
mod park;
mod pins;
mod plugins;
mod plugtune;
pub mod print;
mod reads;
mod rehome;
mod setup;
mod slots;
mod spaces;
mod state;
mod statusline;
mod subagent;
mod term;
mod theme;
mod tmuxconf;
mod tools;
mod trace;
mod trace_claude;
mod trace_codex;
mod trace_page;
mod trash;
mod trust;
mod ui;
mod undo;
mod usage;
mod workdir;

use std::io::{IsTerminal, Write};

use aca_core::i18n::Tr;
use aca_core::{t, tr};

/// Details of every key in the interface (`aca help keys`). The interface's `?` is a separate one-screen cheat sheet (help.rs).
/// Long-form docs have one copy per language: when you change keys.txt, change keys.en.txt too
pub const KEYS: Tr = Tr::new(include_str!("keys.txt"), include_str!("keys.en.txt"));

/// `aca group rename`: the same code as `r` on a group in the interface (the places the interface remembers group names
/// change along with it)
pub use groups::rename_all as rename_group;

/// `aca show --json`'s `work`: the directory the session is really working in (the same one the interface's diff page uses)
pub use data::work_dir;

/// Go into this tmux session (`tmux` is the session id): if already inside tmux, switch this client to it
fn attach_now(tmux: &str) -> bool {
    if std::env::var_os("TMUX").is_some() {
        aca_core::tmux::bind_detach();
        let _ = crate::data::switch_to(tmux);
        std::process::exit(0);
    }
    crate::data::attach(tmux)
}

/// Take out `--flag value` (both are removed from args so the value isn't taken as a keyword)
fn take_opt(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    args.remove(i);
    (i < args.len()).then(|| args.remove(i))
}

/// The list used without the interface (`-l` / straight in by keyword): with pins, notes, seen marks and workspaces, on the
/// last workspace
fn loaded(kw: &str) -> ui::App {
    ui::App::new(kw.to_string(), data::collect())
        .with_pins(pins::Pins::open(pins::Pins::default_path()))
        .with_notes(notes::Notes::open(notes::Notes::default_path()))
        .with_marks(reads::Reads::open(reads::Reads::default_path()), spaces::Spaces::open(spaces::Spaces::default_path()))
        .with_trash(trash::Trash::open(trash::Trash::default_path()))
        .in_space(state::state_file("view").map_or(spaces::HOME, |p| state::View::load(&p).space))
}

fn print_list(kw: &str) {
    let app = loaded(kw);
    let (rows, hidden) = app.visible();
    let width = ratatui::crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .ok()
        .or_else(|| std::env::var("COLUMNS").ok()?.parse().ok())
        .unwrap_or(50);
    let color = std::io::stdout().is_terminal();
    let mut lines: Vec<String> = ui::list_rows(&rows, app.sectioned(), width, None, app.fold_info(), app.who_width(), false)
        .into_iter()
        .flat_map(|r| r.lines)
        .map(|segs| segs.iter().map(|(t, st)| if color { ansi(t, *st) } else { t.clone() }).collect())
        .collect();
    if rows.is_empty() {
        lines.push(if kw.is_empty() {
            t!("tmux 中没有会话", "no sessions in tmux").to_string()
        } else {
            tr!("没有匹配「{kw}」的会话", "nothing matches “{kw}”")
        });
    }
    if hidden > 0 {
        lines.push(tr!("还有 {hidden} 个更早的", "{hidden} more, older"));
    }
    // Piped into head or the like that closes the pipe early: exit quietly, don't panic
    let mut out = std::io::stdout().lock();
    for l in lines {
        if writeln!(out, "{l}").is_err() {
            return;
        }
    }
}

/// aca cost: print the billing panel's content (including the parts the panel only shows after m)
fn print_cost(range: Option<&str>, only: &[String]) {
    let sel = match range {
        Some("1" | "today") => 0,
        Some("30") => 2,
        Some("all") => 3,
        _ => 1,
    };
    let r = usage::build_cached(only);
    let width = ratatui::crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .ok()
        .or_else(|| std::env::var("COLUMNS").ok()?.parse().ok())
        .unwrap_or(80);
    let color = std::io::stdout().is_terminal();
    let mut out = std::io::stdout().lock();
    // First say which accounts alone were counted (so a name that matched nothing shows too)
    if !only.is_empty() {
        let names: Vec<&str> =
            r.accounts.iter().filter(|a| r.ranges[3].by_src.iter().any(|b| b.name == a.name)).map(|a| a.name.as_str()).collect();
        let head = if names.is_empty() {
            tr!(
                " 找不到账号 {}(现有:{})",
                " no account named {} (there are: {})",
                only.join(" / "),
                r.accounts.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(" / ")
            )
        } else {
            tr!(" 仅显示 {}", " only {}", names.join(" + "))
        };
        let _ = writeln!(out, "{head}\n");
    }
    for l in bill::lines(&r, sel, true, width) {
        let s: String = l.spans.iter().map(|sp| if color { ansi(&sp.content, sp.style) } else { sp.content.to_string() }).collect();
        if writeln!(out, "{}", s.trim_end()).is_err() {
            return;
        }
    }
}

/// The terminal's own 16 colors → foreground code
fn named(c: ratatui::style::Color) -> Option<&'static str> {
    use ratatui::style::Color::*;
    Some(match c {
        Black => "30",
        Red => "31",
        Green => "32",
        Yellow => "33",
        Blue => "34",
        Magenta => "35",
        Cyan => "36",
        Gray => "37",
        DarkGray => "90",
        LightRed => "91",
        LightGreen => "92",
        LightYellow => "93",
        LightBlue => "94",
        LightMagenta => "95",
        LightCyan => "96",
        White => "97",
        _ => return None,
    })
}

fn ansi(t: &str, st: ratatui::style::Style) -> String {
    use ratatui::style::{Color, Modifier};
    let mut codes = vec![];
    if st.add_modifier.contains(Modifier::BOLD) {
        codes.push("1");
    }
    if st.add_modifier.contains(Modifier::DIM) {
        codes.push("2");
    }
    let fg;
    match st.fg {
        Some(Color::Indexed(n)) => {
            fg = format!("38;5;{n}");
            codes.push(&fg);
        }
        // State colors (theme.rs)
        Some(Color::Rgb(r, g, b)) => {
            fg = format!("38;2;{r};{g};{b}");
            codes.push(&fg);
        }
        // The terminal's own 16 colors (color names written in the theme file)
        Some(c) => codes.extend(named(c)),
        None => {}
    }
    // Billing's split bars: within one cell the first segment is the foreground, the second the background
    let bg;
    match st.bg {
        Some(Color::Indexed(n)) => {
            bg = format!("48;5;{n}");
            codes.push(&bg);
        }
        Some(Color::Rgb(r, g, b)) => {
            bg = format!("48;2;{r};{g};{b}");
            codes.push(&bg);
        }
        _ => {}
    }
    if codes.is_empty() { t.to_string() } else { format!("\x1b[{}m{t}\x1b[0m", codes.join(";")) }
}

/// Which accounts alone billing counts: `--only` can be given several times, or comma-separated
fn take_only(args: &mut Vec<String>) -> Vec<String> {
    let mut only: Vec<String> = vec![];
    while let Some(o) = take_opt(args, "--only") {
        only.extend(o.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from));
    }
    only
}

/// The commands outside the interface that share its code: `aca cost | limits | statusline | setup | account | theme | tmux |
/// next …`. Anything else = None
pub fn command(name: &str, args: &[String]) -> Option<i32> {
    Some(match name {
        // statusLine runs it on every refresh: touches nothing else; after recording the quota it hands the input to the next command
        "limits" => limits::run(args),
        // Also runs on every refresh: after recording the quota it draws the status bar itself, handing nothing to another command
        "statusline" => statusline::run(),
        // tmux's status bar runs it every few seconds: read-only, which sessions need you
        "tmux-status" => bar::run(args),
        // The sessions the status bar's two numbers count: go to the next one (bound to a key, or typed yourself)
        "next" => bar::next(args),
        // The switcher keys and the status bar segment go into aca's own tmux config; your tmux.conf only gains one source-file line
        "tmux" => tmuxconf::run(args),
        // What to decide after installing: register accounts, whether to wire Claude's status bar (a table in the terminal)
        "setup" => setup::run(args),
        "account" => setup::account(args),
        "theme" => theme::run(args, std::io::stdout().is_terminal()),
        "cost" => {
            let mut args = args.to_vec();
            let only = take_only(&mut args);
            print_cost(args.first().map(String::as_str), &only);
            0
        }
        _ => return None,
    })
}

/// `aca ui …` (plain `aca` with no arguments too)
pub fn main(mut args: Vec<String>) -> anyhow::Result<()> {
    if let Some(c) = take_opt(&mut args, "--client") {
        data::set_client(c);
    }
    // When the popup takes only one side of the screen: wherever the highlight moves, this client switches there (see follow.rs)
    if args.iter().any(|a| a == "--follow") {
        follow::ask();
    }
    // Passed by the interface's own exec when ^L restarts it: where to open (list / bill: the time range); not for people
    let restarted = take_opt(&mut args, "--restarted");
    let only = take_only(&mut args);
    // On exit the interface starts this child process to really delete what has expired in the trash (see trash.rs)
    if args.iter().any(|a| a == "--purge") {
        trash::purge();
        return Ok(());
    }
    // $ in the interface starts this child process to do the reading (it takes a lot of memory, freed when done); the result
        // comes back as JSON
    if args.iter().any(|a| a == "--cost-json") {
        let r = usage::build_cached(&only);
        let _ = serde_json::to_writer(std::io::stdout().lock(), &r);
        return Ok(());
    }
    if let Some(i) = args.iter().position(|a| a == "--snapshot") {
        let (w, h) =
            args.get(i + 1).and_then(|s| s.split_once('x')).and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))).unwrap_or((46, 30));
        let _ = write!(std::io::stdout(), "{}", ui::snapshot(w, h, args.get(i + 2).map(String::as_str).unwrap_or(""))?);
        return Ok(());
    }
    let dry = args.iter().any(|a| a == "-n");
    let list_only = args.iter().any(|a| a == "-l");
    let kw = args.iter().filter(|a| !a.starts_with('-')).cloned().collect::<Vec<_>>().join(" ");
    let tty = std::io::stdout().is_terminal() && std::io::stdin().is_terminal();

    if list_only || (!tty && !dry) {
        print_list(&kw);
        return Ok(());
    }
    let mut filter = kw.clone();
    if !kw.is_empty() {
        // Only look in the workspace you were last on (other workspaces, h especially, are neither entered directly nor printed)
        let app = loaded("");
        let hits = app.matching(&kw);
        if dry {
            match hits.as_slice() {
                [one] => println!("{}\t{}", one.name, one.title),
                [] => println!("{}", tr!("没有匹配「{kw}」的会话", "no session matches “{kw}”")),
                many => many.iter().for_each(|s| println!("{}\t{}", s.name, s.title)),
            }
            return Ok(());
        }
        if let [one] = hits.as_slice() {
            attach_now(&one.tmux);
            filter.clear(); // back from detach, show the full list
        }
    }
    ui::run(filter, restarted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printed_colors_keep_the_background_of_split_cells() {
        use ratatui::style::{Color, Style};
        let st = Style::new().fg(Color::Rgb(1, 2, 3)).bg(Color::Rgb(4, 5, 6));
        assert_eq!(ansi("▌", st), "\x1b[38;2;1;2;3;48;2;4;5;6m▌\x1b[0m");
        assert_eq!(ansi("x", Style::new()), "x");
        // Color names written in a theme (blue, darkgray) carry over too
        assert_eq!(ansi("x", Style::new().fg(Color::Blue)), "\x1b[34mx\x1b[0m");
        assert_eq!(ansi("x", Style::new().fg(Color::DarkGray)), "\x1b[90mx\x1b[0m");
        assert_eq!(ansi("x", Style::new().fg(Color::Reset)), "x");
    }

    #[test]
    fn client_is_taken_out_before_the_keyword() {
        let mut a: Vec<String> = ["--client", "/dev/ttys003", "foo"].map(String::from).to_vec();
        assert_eq!(take_opt(&mut a, "--client").as_deref(), Some("/dev/ttys003"));
        assert_eq!(a, ["foo"]);
        let mut a: Vec<String> = ["foo", "--client"].map(String::from).to_vec();
        assert_eq!(take_opt(&mut a, "--client"), None); // no value given
        assert_eq!(a, ["foo"]);
        assert_eq!(take_opt(&mut a, "--client"), None);
    }
}
