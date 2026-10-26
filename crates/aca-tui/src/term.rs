//! t:aca 底部的临时终端。自己开一个 pty 跑 shell,vt100 解析它的输出、画在列表下面,按键编码成终端字节送进去。
//! Esc 收起(shell 接着跑,再按 t 回到它),里面 exit 才真的关;aca 退出时一起结束。不经过 tmux,不留任何东西

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

/// 有终端出了新输出:run 循环据此提前醒来重画
pub static DIRTY: AtomicBool = AtomicBool::new(false);

pub struct Term {
    /// 开在哪、给哪个会话开的(终端顶上那行写着)
    pub dir: String,
    pub title: String,
    parser: Arc<Mutex<vt100::Parser<Replies>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16),
    /// 登录终端:盯着登录链接(见 [`Term::new_link`])
    pub link: Option<Catch>,
    /// 已经交出去弹过的那个链接
    linked: Option<Link>,
}

/// 登录链接从哪来:Claude 交给 aca 的写在 file 里(见 `aca_core::agent::login_link`);
/// Codex 不交(macOS 上它不认 BROWSER)、远程时 Claude 也不交,screen = 从屏幕上认
#[derive(Clone, Debug, PartialEq)]
pub struct Catch {
    pub file: std::path::PathBuf,
    pub screen: bool,
}

/// 登录链接;code = Codex 设备码登录给的一次性码(打开链接后在网页里输入)
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub url: String,
    pub code: Option<String>,
}

/// 程序问终端的话(设备属性、光标在哪)由我们替终端答:vt100 只解析不回话,
/// 不答的话 zsh 提示符插件之类会等到超时
#[derive(Default)]
struct Replies(Vec<u8>);

impl vt100::Callbacks for Replies {
    fn unhandled_csi(&mut self, screen: &mut vt100::Screen, i1: Option<u8>, _: Option<u8>, params: &[&[u16]], c: char) {
        let p = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c, p) {
            (None, 'c', 0) => self.0.extend_from_slice(b"\x1b[?62;22c"),
            (Some(b'>'), 'c', 0) => self.0.extend_from_slice(b"\x1b[>0;0;0c"),
            (None, 'n', 5) => self.0.extend_from_slice(b"\x1b[0n"),
            (None, 'n', 6) => {
                let (r, c) = screen.cursor_position();
                self.0.extend_from_slice(format!("\x1b[{};{}R", r + 1, c + 1).as_bytes());
            }
            _ => {}
        }
    }
}

impl Term {
    /// 在 dir 开一个登录 shell($SHELL -l);env 是要额外带上的(账号的配置目录),会话 id 一律去掉
    pub fn spawn(dir: &str, env: &[(String, String)], rows: u16, cols: u16) -> anyhow::Result<Term> {
        let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
        Term::spawn_argv(&[&shell, "-l"], dir, env, rows, cols)
    }

    /// 在 dir 用登录 shell 跑一条命令(`$SHELL -lc`,PATH 和平时一样),它退出终端就关
    pub fn spawn_cmd(cmd: &str, dir: &str, env: &[(String, String)], rows: u16, cols: u16) -> anyhow::Result<Term> {
        let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
        Term::spawn_argv(&[&shell, "-lc", cmd], dir, env, rows, cols)
    }

    fn spawn_argv(argv: &[&str], dir: &str, env: &[(String, String)], rows: u16, cols: u16) -> anyhow::Result<Term> {
        let (rows, cols) = (rows.max(2), cols.max(10));
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        let mut cmd = CommandBuilder::from_argv(argv.iter().map(Into::into).collect());
        cmd.cwd(dir);
        for (k, _) in std::env::vars_os() {
            if k.to_str().is_some_and(|k| crate::data::session_id_var(k) || k == "TMUX_PANE") {
                cmd.env_remove(k);
            }
        }
        cmd.env("TERM", "xterm-256color");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(rows, cols, 0, Replies::default())));
        {
            let (parser, writer) = (parser.clone(), writer.clone());
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                // shell 退出后读到 0 或 EIO,线程就结束
                while let Ok(n @ 1..) = reader.read(&mut buf) {
                    let reply = {
                        let mut p = parser.lock().unwrap();
                        p.process(&buf[..n]);
                        std::mem::take(&mut p.callbacks_mut().0)
                    };
                    if !reply.is_empty() {
                        let _ = writer.lock().unwrap().write_all(&reply);
                    }
                    DIRTY.store(true, Ordering::Relaxed);
                }
                DIRTY.store(true, Ordering::Relaxed);
            });
        }
        Ok(Term {
            dir: dir.to_string(),
            title: String::new(),
            parser,
            writer,
            master: pair.master,
            child,
            size: (rows, cols),
            link: None,
            linked: None,
        })
    }

    /// 登录链接出来了、还没交出去过:交出来(界面弹出来挑浏览器)。同一个链接只交一次,再登一次换了链接再交
    pub fn new_link(&mut self) -> Option<Link> {
        let c = self.link.as_ref()?;
        let link = std::fs::read_to_string(&c.file)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| s.starts_with("http"))
            .map(|url| Link { url, code: None })
            .or_else(|| if c.screen { link_on(&self.text()) } else { None })?;
        if self.linked.as_ref() == Some(&link) {
            return None;
        }
        self.linked = Some(link.clone());
        Some(link)
    }

    /// 跟着画它的那块变大小(shell 收到 SIGWINCH 自己重排)
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(10));
        if (rows, cols) == self.size {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.parser.lock().unwrap().screen_mut().set_size(rows, cols);
    }

    pub fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    pub fn key(&mut self, k: KeyEvent) {
        let app_cursor = self.parser.lock().unwrap().screen().application_cursor();
        let b = encode(k, app_cursor);
        self.write(&b);
    }

    fn write(&self, b: &[u8]) {
        if !b.is_empty() {
            let mut w = self.writer.lock().unwrap();
            let _ = w.write_all(b);
            let _ = w.flush();
        }
    }

    /// 画进 area,返回光标该在哪(程序藏了光标就没有)
    pub fn render(&self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        let p = self.parser.lock().unwrap();
        let s = p.screen();
        let (rows, cols) = s.size();
        for r in 0..rows.min(area.height) {
            for c in 0..cols.min(area.width) {
                let Some(cell) = s.cell(r, c) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let sym = if cell.has_contents() { cell.contents() } else { " " };
                buf.set_stringn(area.x + c, area.y + r, sym, (area.width - c) as usize, style(cell));
            }
        }
        if s.hide_cursor() || area.width == 0 || area.height == 0 {
            return None;
        }
        let (r, c) = s.cursor_position();
        Some(Position::new(area.x + c.min(area.width - 1), area.y + r.min(area.height - 1)))
    }

    fn text(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }
}

/// 屏幕上的登录链接(折行接起来的整条):授权页那个(`…/oauth/authorize?…`),
/// 或者 Codex 设备码登录的那个(`…/device`)和后面跟着的一次性码(`ABCD-EFGHI`)
fn link_on(screen: &str) -> Option<Link> {
    let words: Vec<&str> = screen.split_whitespace().collect();
    let at = words.iter().position(|w| w.starts_with("https://") && (w.contains("/oauth/authorize?") || w.ends_with("/device")))?;
    let url = words[at].to_string();
    if !url.ends_with("/device") {
        // 后面还有字了才算打完(Claude 跟着「Paste code here」,Codex 跟着下一行提示),免得认到打了一半的
        return (at + 1 < words.len()).then_some(Link { url, code: None });
    }
    // 码还没打出来就先不交,下一轮再看
    let code = words[at + 1..].iter().find(|w| one_time_code(w))?;
    Some(Link { url, code: Some(code.to_string()) })
}

fn one_time_code(w: &str) -> bool {
    let Some((a, b)) = w.split_once('-') else { return false };
    a.len() == 4 && (4..=6).contains(&b.len()) && w.chars().all(|c| c == '-' || c.is_ascii_uppercase() || c.is_ascii_digit())
}

impl Drop for Term {
    // shell 是会话首进程:它一死,前台还在跑的命令收到 SIGHUP
    fn drop(&mut self) {
        // kill 先发 SIGHUP、等一会儿还在才 SIGKILL,后一种它不收:收掉,不然留个僵尸
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(c) = &self.link {
            let _ = std::fs::remove_file(&c.file);
        }
    }
}

fn style(cell: &vt100::Cell) -> Style {
    let color = |c| match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    };
    let mut st = Style::new().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
    for (on, m) in [
        (cell.bold(), Modifier::BOLD),
        (cell.dim(), Modifier::DIM),
        (cell.italic(), Modifier::ITALIC),
        (cell.underline(), Modifier::UNDERLINED),
        (cell.inverse(), Modifier::REVERSED),
    ] {
        if on {
            st = st.add_modifier(m);
        }
    }
    st
}

/// 按键 → 终端字节(xterm 的写法)。app_cursor:程序开了「应用光标键」模式(vim 之类),方向键发 ESC O x
fn encode(k: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let (ctrl, alt, shift) =
        (k.modifiers.contains(KeyModifiers::CONTROL), k.modifiers.contains(KeyModifiers::ALT), k.modifiers.contains(KeyModifiers::SHIFT));
    // 带修饰的方向键等:ESC [ 1 ; m x,m = 1 + shift + 2·alt + 4·ctrl
    let m = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let cursor = |x: char| -> Vec<u8> {
        match (m, app_cursor) {
            (1, true) => format!("\x1bO{x}"),
            (1, false) => format!("\x1b[{x}"),
            _ => format!("\x1b[1;{m}{x}"),
        }
        .into_bytes()
    };
    let tilde = |n: u8| -> Vec<u8> { if m == 1 { format!("\x1b[{n}~") } else { format!("\x1b[{n};{m}~") }.into_bytes() };
    let mut out = match k.code {
        KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
            c @ 'a'..='z' => vec![c as u8 & 0x1f],
            '@' | ' ' | '2' => vec![0],
            '[' | '3' => vec![0x1b],
            '\\' | '4' => vec![0x1c],
            ']' | '5' => vec![0x1d],
            '^' | '6' => vec![0x1e],
            '_' | '-' | '7' => vec![0x1f],
            '?' | '8' => vec![0x7f],
            c => c.to_string().into_bytes(),
        },
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace if ctrl => vec![0x08],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => cursor('A'),
        KeyCode::Down => cursor('B'),
        KeyCode::Right => cursor('C'),
        KeyCode::Left => cursor('D'),
        KeyCode::Home => cursor('H'),
        KeyCode::End => cursor('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) if m == 1 => format!("\x1bO{}", (b'P' + n - 1) as char).into_bytes(),
        KeyCode::F(n @ 1..=4) => format!("\x1b[1;{m}{}", (b'P' + n - 1) as char).into_bytes(),
        KeyCode::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize]),
        _ => vec![],
    };
    // Alt+字母这类:前面垫一个 ESC(方向键等的 alt 已经编进 m 里)
    let plain = matches!(k.code, KeyCode::Char(_) | KeyCode::Enter | KeyCode::Tab | KeyCode::Backspace | KeyCode::Esc);
    if alt && plain && !out.is_empty() {
        out.insert(0, 0x1b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn k(code: KeyCode, m: KeyModifiers) -> Vec<u8> {
        encode(KeyEvent::new(code, m), false)
    }

    #[test]
    fn keys_turn_into_what_a_terminal_would_send() {
        let n = KeyModifiers::NONE;
        assert_eq!(k(KeyCode::Char('中'), n), "中".as_bytes());
        assert_eq!(k(KeyCode::Char('c'), KeyModifiers::CONTROL), [3]);
        assert_eq!(k(KeyCode::Char('4'), KeyModifiers::CONTROL), [0x1c]); // 不带扩展键盘时 ^\ 长这样
        assert_eq!(k(KeyCode::Char('b'), KeyModifiers::ALT), b"\x1bb");
        assert_eq!(k(KeyCode::Enter, n), b"\r");
        assert_eq!(k(KeyCode::Backspace, n), [0x7f]);
        assert_eq!(k(KeyCode::Up, n), b"\x1b[A");
        assert_eq!(encode(KeyEvent::new(KeyCode::Up, n), true), b"\x1bOA"); // vim 之类开了应用光标键
        assert_eq!(k(KeyCode::Left, KeyModifiers::CONTROL), b"\x1b[1;5D");
        assert_eq!(k(KeyCode::Delete, n), b"\x1b[3~");
        assert_eq!(k(KeyCode::F(1), n), b"\x1bOP");
        assert_eq!(k(KeyCode::F(5), n), b"\x1b[15~");
    }

    #[test]
    fn questions_to_the_terminal_get_answered() {
        let mut p = vt100::Parser::new_with_callbacks(5, 20, 0, Replies::default());
        p.process(b"ab\x1b[c\x1b[6n");
        assert_eq!(p.callbacks().0, b"\x1b[?62;22c\x1b[1;3R");
    }

    fn wait_for(t: &Term, want: &str) -> String {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let s = t.text();
            if s.contains(want) || Instant::now() > end {
                return s;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_real_shell_runs_in_the_dir_and_draws_in_color() {
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let dir = dir.to_string_lossy();
        let env = [("ACA_T_TEST".to_string(), "带上了".to_string())];
        let mut t = Term::spawn_argv(&["/bin/sh"], &dir, &env, 6, 40).unwrap();
        // 键一个个送进去,和在 aca 里敲的一样
        for c in "printf '\\033[31mred\\033[0m %s ' \"$ACA_T_TEST\"; pwd\r".chars() {
            t.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let s = wait_for(&t, &dir);
        assert!(s.contains("red 带上了") && s.contains(&*dir), "{s}");
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 6));
        assert!(t.render(buf.area, &mut buf).is_some());
        // 命令回显里也有 red,找后面跟着「 带」的那个(输出)
        let at = (0..6).find_map(|y| {
            (0..36)
                .find(|&x| ["r", "e", "d", " ", "带"].iter().enumerate().all(|(i, w)| buf[(x + i as u16, y)].symbol() == *w))
                .map(|x| (x, y))
        });
        let (x, y) = at.expect("画出来了");
        assert_eq!(buf[(x, y)].fg, Color::Indexed(1)); // 红色
        t.resize(8, 50);
        assert_eq!(t.parser.lock().unwrap().screen().size(), (8, 50));
        t.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)); // ^D 退出
        let end = Instant::now() + Duration::from_secs(5);
        while !t.exited() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(t.exited());
    }

    #[test]
    fn a_login_link_is_handed_over_once_from_the_screen_or_the_file() {
        let tmp = std::env::temp_dir().canonicalize().unwrap();
        let file = tmp.join(format!("aca-term-link-{}.url", std::process::id()));
        let _ = std::fs::remove_file(&file);
        // Codex 只打在屏幕上:窄终端里折成好几行,接起来是整条;前面别的地址不算
        let url = format!("https://auth.openai.com/oauth/authorize?response_type=code&state={}", "s".repeat(70));
        let say = format!(
            "echo 'Starting local login server on http://localhost:1455.'; echo 'https://example.com/docs'; echo '{url}'; echo 'On a remote machine?'; sleep 5"
        );
        let mut t = Term::spawn_argv(&["/bin/sh", "-c", &say], &tmp.to_string_lossy(), &[], 10, 40).unwrap();
        t.link = Some(Catch { file: file.clone(), screen: true });
        wait_for(&t, "ssss");
        assert_eq!(t.new_link(), Some(Link { url: url.clone(), code: None }));
        // 交过一次就不再交
        assert_eq!(t.new_link(), None);
        // Claude 交给 aca 的写在文件里:先认文件;不认屏幕的(本机时 Claude 屏幕上那条要贴码)只看文件
        std::fs::write(&file, "https://claude.com/cai/oauth/authorize?code=true&x=1\n").unwrap();
        t.link = Some(Catch { file: file.clone(), screen: false });
        assert_eq!(t.new_link().map(|l| l.url).as_deref(), Some("https://claude.com/cai/oauth/authorize?code=true&x=1"));
        assert_eq!(t.new_link(), None);
        // 关掉终端,链接文件一起删
        drop(t);
        assert!(!file.exists());
    }

    #[test]
    fn a_device_code_login_hands_over_the_link_with_its_one_time_code() {
        // Codex 远程时走 --device-auth:固定链接,下面一行是一次性码;码出来之前不交
        let half = "Follow these steps to sign in with ChatGPT using device code authorization:\n\
                    1. Open this link in your browser and sign in to your account\n   https://auth.openai.com/codex/device\n";
        assert_eq!(link_on(half), None);
        let whole = format!("{half}2. Enter this one-time code (expires in 15 minutes)\n   AB12-CD3EF\nContinue only if you started this login.");
        assert_eq!(
            link_on(&whole),
            Some(Link { url: "https://auth.openai.com/codex/device".into(), code: Some("AB12-CD3EF".into()) })
        );
        // 授权页的链接后面就算有像码的字也不当码;链接是屏幕上最后一个字时还没打完,不交
        assert_eq!(link_on("visit https://x.ai/oauth/authorize?a=1 then AB12-CD3EF").and_then(|l| l.code), None);
        assert_eq!(link_on("visit https://x.ai/oauth/authorize?a=1&st"), None);
        assert!(one_time_code("W2U9-CRA4P") && !one_time_code("2.1-299") && !one_time_code("ab12-cd3e"));
    }
}
