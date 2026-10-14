//! aca 的界面:一屏列出所有会话(登记的、普通 tmux 的),看高亮那个的实时画面,回车进去。
//!
//! 数据都从 aca-core 来(登记表、hook 写的状态、tmux 里实际在的窗格),对会话的操作也都是调它的函数;
//! 这里只管怎么列、怎么画、按键做什么,和几样自己的小状态(置顶、便签、已读、工作区,见 state.rs)。

mod accounts;
mod acctpage;
mod ansi;
mod bar;
mod bill;
mod browser;
mod caps;
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

/// 界面里每个键的细节(`aca help keys`)。界面里的 `?` 是另一张一屏的速查表(help.rs)。
/// 成篇的说明一种话一份:改了 keys.txt 记得改 keys.en.txt
pub const KEYS: Tr = Tr::new(include_str!("keys.txt"), include_str!("keys.en.txt"));

/// `aca group rename`:和界面里分组上 `r` 改名是同一份(界面记着组名的地方一起改)
pub use groups::rename_all as rename_group;

/// 进这个 tmux 会话(`tmux` 是会话号):已经在 tmux 里就把这台客户端切过去
fn attach_now(tmux: &str) -> bool {
    if std::env::var_os("TMUX").is_some() {
        let _ = crate::data::switch_to(tmux);
        std::process::exit(0);
    }
    crate::data::attach(tmux)
}

/// 取出 `--flag 值`(两个都从 args 里拿掉,免得值被当成关键词)
fn take_opt(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    args.remove(i);
    (i < args.len()).then(|| args.remove(i))
}

/// 不进界面时(`-l` / 关键词直达)用的列表:接上置顶、便签、已读、工作区,停在上次的工作区
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
            t!("tmux 里没有会话", "no sessions in tmux").to_string()
        } else {
            tr!("没有匹配「{kw}」的", "nothing matches “{kw}”")
        });
    }
    if hidden > 0 {
        lines.push(tr!("还有 {hidden} 个更早的", "{hidden} more, older"));
    }
    // 接到 head 之类提前关管道时安静退出,别 panic
    let mut out = std::io::stdout().lock();
    for l in lines {
        if writeln!(out, "{l}").is_err() {
            return;
        }
    }
}

/// aca cost:把计费面板的内容打印出来
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
    // 只算了哪些账号先说一声(对不上名字的也看得出来)
    if !only.is_empty() {
        let names: Vec<&str> =
            r.accounts.iter().filter(|a| r.ranges[3].by_src.iter().any(|b| b.name == a.name)).map(|a| a.name.as_str()).collect();
        let head = if names.is_empty() {
            tr!(
                " 没有叫 {} 的账号(有:{})",
                " no account named {} (there are: {})",
                only.join(" / "),
                r.accounts.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(" / ")
            )
        } else {
            tr!(" 只看 {}", " only {}", names.join(" + "))
        };
        let _ = writeln!(out, "{head}\n");
    }
    for l in bill::lines(&r, sel, width) {
        let s: String = l.spans.iter().map(|sp| if color { ansi(&sp.content, sp.style) } else { sp.content.to_string() }).collect();
        if writeln!(out, "{}", s.trim_end()).is_err() {
            return;
        }
    }
}

/// 终端自己的 16 色 → 前景色的码
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
        // 状态色(theme.rs)
        Some(Color::Rgb(r, g, b)) => {
            fg = format!("38;2;{r};{g};{b}");
            codes.push(&fg);
        }
        // 终端自己的 16 色(主题文件里写的颜色名)
        Some(c) => codes.extend(named(c)),
        None => {}
    }
    // 计费的分段条:一格里前一段是前景、后一段是背景
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

/// 计费只算哪些账号:`--only` 可以给多次,也可以逗号隔开
fn take_only(args: &mut Vec<String>) -> Vec<String> {
    let mut only: Vec<String> = vec![];
    while let Some(o) = take_opt(args, "--only") {
        only.extend(o.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from));
    }
    only
}

/// 界面之外、和界面共用代码的几条命令:`aca cost | limits | statusline | setup | account | theme | tmux | next …`。不是这几条 = None
pub fn command(name: &str, args: &[String]) -> Option<i32> {
    Some(match name {
        // statusLine 每次刷新都跑:别的都不碰,记完额度就把输入交给后面的命令
        "limits" => limits::run(args),
        // 同样每次刷新都跑:记完额度自己把状态栏画出来,不再交给别的命令
        "statusline" => statusline::run(),
        // tmux 的状态栏每隔几秒跑一次:只读,哪些会话在等你
        "tmux-status" => bar::run(args),
        // 状态栏上那两个数指的那些:进下一个(绑在一个键上,自己敲也行)
        "next" => bar::next(args),
        // 切换器的键、状态栏那一段写进 aca 自己的一份 tmux 配置,你的 tmux.conf 里只加一行 source-file
        "tmux" => tmuxconf::run(args),
        // 装完之后要定的:登记账号、接不接 Claude 的状态栏(在终端里是一张表)
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

/// `aca ui …`(不带参数的 `aca` 也是它)
pub fn main(mut args: Vec<String>) -> anyhow::Result<()> {
    if let Some(c) = take_opt(&mut args, "--client") {
        data::set_client(c);
    }
    // 弹窗只占屏幕一边时:高亮挪到哪,这台客户端就切到哪(见 follow.rs)
    if args.iter().any(|a| a == "--follow") {
        follow::ask();
    }
    // 界面里 ^L 重启时 exec 自己带上的:开到哪(list / bill:时段),不给人用
    let restarted = take_opt(&mut args, "--restarted");
    let only = take_only(&mut args);
    // 退出的界面起这个子进程去真删它删掉的会话(见 trash.rs)
    if args.iter().any(|a| a == "--purge") {
        trash::purge();
        return Ok(());
    }
    // 界面里的 $ 起这个子进程去读(读的时候内存大,读完就还),结果以 JSON 交回
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
        // 只找上次停在的工作区里的(别的工作区,尤其 h,不直接进也不打印)
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
            filter.clear(); // detach 回来显示完整列表
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
        // 主题里写颜色名的(blue、darkgray)也带得出去
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
        assert_eq!(take_opt(&mut a, "--client"), None); // 没给值
        assert_eq!(a, ["foo"]);
        assert_eq!(take_opt(&mut a, "--client"), None);
    }
}
