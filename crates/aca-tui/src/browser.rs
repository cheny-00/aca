//! A login link pops up a list to pick how to open it (the user, 2026-10-14: a login shouldn't jump to the default browser
//! on its own; that isn't necessarily the browser for this account).
//! Logins aca starts (⏎ on the accounts page, a newly added account) don't let the tool open a browser itself (see
//! `aca_core::agent::quiet_login`); the link comes to the interface instead:
//! the first item is 「复制链接」 (copy the link; works on a server or when connected from a phone), then the installed
//! browsers (the default one first, marked).
//! Remote (`aca_core::agent::remote`): no browsers listed, they would open on this machine; the link becomes one you can
//! finish on another device (Claude has you paste the code from the web page back into the terminal; Codex uses a device
//! code: the list shows the one-time code, also copyable). Asks every time, remembers nothing.
//! Esc: don't open it for now; the link is still in the login terminal and the tool keeps waiting.
//! A browser login that waits on this machine for the browser to come back (Codex's `http://localhost:1455/auth/callback`)
//! can be finished on another computer too: 「在别的电脑登录」 (log in on another computer) copies the link, then takes the
//! address that browser landed on, pasted back, and makes the same request to the login waiting here.
use std::process::Command;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::{Line, Span};

use aca_core::{t, tr};

use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::term::Link;
use crate::theme;

/// Browsers we recognize (.app names on macOS); lists whichever are installed
const KNOWN: &[&str] = &[
    "Safari",
    "Google Chrome",
    "Firefox",
    "Arc",
    "Dia",
    "Zen",
    "Zen Browser",
    "Microsoft Edge",
    "Brave Browser",
    "Vivaldi",
    "Opera",
    "Orion",
    "Chromium",
    "ChatGPT Atlas",
    "Comet",
    "LibreWolf",
    "Floorp",
    "Waterfox",
    "DuckDuckGo",
    "Tor Browser",
    "Google Chrome Canary",
    "Firefox Developer Edition",
    "Firefox Nightly",
];

/// What to do once picked
#[derive(Clone, Debug, PartialEq)]
pub enum How {
    Copy,
    /// Copy the one-time code of a device-code login
    CopyCode,
    /// Copy the link, then take the address the browser on the other computer landed on
    Elsewhere,
    /// Open with this .app (Mac)
    App(String),
    /// Run this command: the link goes last, or replaces `{url}` if an argument has it (from `[login] browsers`, or a Linux .desktop)
    Cmd(Vec<String>),
    /// The system's default browser (when we can't tell which are installed)
    System,
}

struct Item {
    label: String,
    how: How,
}

/// A browser found automatically: name, how to open it, its id (bundle id on Mac, .desktop file name on Linux; goes first if it matches the
/// default browser)
struct Found {
    name: String,
    how: How,
    id: String,
}

pub struct Pick {
    /// Which login (the login terminal's title: 「登录 Claude Code · ding」)
    title: String,
    link: Link,
    /// The user isn't at this machine: no browsers listed
    far: bool,
    items: Vec<Item>,
    sel: usize,
    open: fn(&How, &str) -> Result<(), String>,
    /// Where this login waits for the browser to come back ([`callback_of`]); None = not a login that does
    back: Option<String>,
    /// Taking the address pasted back: what's typed so far, and why the last one didn't go
    paste: Option<(String, String)>,
    /// Hands the pasted address to the login ([`deliver`]; swapped out in tests)
    send: fn(&str, &str) -> Result<(), String>,
}

impl Pick {
    /// Which browsers to list: none when remote; only those in config.toml's `[login] browsers` if set; otherwise found automatically (.app
    /// on Mac, .desktop on Linux)
    pub fn new(title: &str, link: Link) -> Pick {
        let cfg = aca_core::config::load();
        let far = aca_core::agent::remote(&cfg);
        let browsers = if far {
            vec![]
        } else if !cfg.login.browsers.is_empty() {
            cfg.login.browsers.iter().map(|b| Item { label: b.name.clone(), how: How::Cmd(b.cmd.clone()) }).collect()
        } else if cfg!(target_os = "macos") {
            arrange(installed(), default_browser())
        } else {
            arrange(desktop_browsers(&app_dirs()), default_desktop())
        };
        Pick::with(title, link, far, browsers, launch)
    }

    fn with(title: &str, link: Link, far: bool, browsers: Vec<Item>, open: fn(&How, &str) -> Result<(), String>) -> Pick {
        let mut items = vec![Item { label: t!("复制链接", "copy the link").into(), how: How::Copy }];
        if let Some(code) = &link.code {
            items.push(Item { label: tr!("复制验证码 {code}", "copy the one-time code {code}"), how: How::CopyCode });
        }
        let back = if link.code.is_none() { callback_of(&link.url) } else { None };
        if back.is_some() {
            items.push(Item { label: t!("在别的电脑登录", "log in on another computer").into(), how: How::Elsewhere });
        }
        items.extend(browsers);
        Pick { title: title.to_string(), link, far, items, sel: 0, open, back, paste: None, send: deliver }
    }

    fn choose(&mut self, i: usize, fx: &mut Fx) -> Flow {
        let Some(it) = self.items.get(i) else { return Flow::Stay };
        let copied = match &it.how {
            How::Copy => self.link.url.clone(),
            How::CopyCode => self.link.code.clone().unwrap_or_default(),
            How::Elsewhere => {
                fx.copy = Some(self.link.url.clone());
                self.paste = Some((String::new(), String::new()));
                return Flow::Stay;
            }
            how => {
                return match (self.open)(how, &self.link.url) {
                    Ok(()) => {
                        fx.say(tr!("已在 {} 打开 · 登录后返回终端", "opened in {} · log in, then back to the terminal", it.label));
                        Flow::Close
                    }
                    Err(e) => {
                        fx.say(e);
                        Flow::Stay
                    }
                };
            }
        };
        fx.copy = Some(copied);
        // A device-code login needs both the link and the code: after copying one, the list stays open to copy the other
        if self.link.code.is_some() { Flow::Stay } else { Flow::Close }
    }

    /// Shown at the top of the list: how to finish logging in
    fn hint(&self) -> Option<&'static str> {
        if self.link.code.is_some() {
            return Some(t!(
                "可在任意设备上打开链接登录,并输入上方验证码(15 分钟内有效)",
                "any device: open the link, log in, enter the code above (15 minutes)"
            ));
        }
        self.far.then(|| {
            t!(
                "在其他设备上打开链接登录,将网页上的验证码粘贴到下方终端",
                "open the link on another device, log in, paste the page's code below"
            )
        })
    }
}

/// Arrange the found browsers into the list: the default one first, marked. If none is recognized, offer the system default
fn arrange(mut found: Vec<Found>, default: Option<String>) -> Vec<Item> {
    let def = default.and_then(|id| found.iter().position(|f| f.id.eq_ignore_ascii_case(id.trim())));
    if let Some(i) = def {
        let d = found.remove(i);
        found.insert(0, d);
    }
    // Of browsers with the same name (firefox and firefox-esr on Linux and the like) keep only the first: the default is already at the
    // front
    let mut names = std::collections::HashSet::new();
    found.retain(|f| names.insert(f.name.clone()));
    let out: Vec<Item> = found
        .into_iter()
        .enumerate()
        .map(|(i, f)| Item {
            label: if def.is_some() && i == 0 { tr!("{}(默认)", "{} (default)", f.name) } else { f.name },
            how: f.how,
        })
        .collect();
    if out.is_empty() {
        return vec![Item { label: t!("默认浏览器", "the default browser").into(), how: How::System }];
    }
    out
}

/// macOS: recognized browsers in /Applications and ~/Applications
fn installed() -> Vec<Found> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dirs = ["/Applications".to_string(), format!("{home}/Applications")];
    KNOWN
        .iter()
        .filter_map(|name| {
            let path = dirs.iter().map(|d| format!("{d}/{name}.app")).find(|p| std::path::Path::new(p).is_dir())?;
            let id = plutil(&["-extract", "CFBundleIdentifier", "raw", "-o", "-", &format!("{path}/Contents/Info.plist")]);
            Some(Found { name: name.to_string(), how: How::App(path), id: id.unwrap_or_default() })
        })
        .collect()
}

/// The directories Linux keeps .desktop files in (per XDG, plus flatpak's and snap's): earlier ones win
fn app_dirs() -> Vec<std::path::PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let var = |k: &str, or: String| std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or(or);
    let mut dirs = vec![var("XDG_DATA_HOME", format!("{home}/.local/share"))];
    dirs.extend(var("XDG_DATA_DIRS", "/usr/local/share:/usr/share".into()).split(':').map(String::from));
    dirs.extend([format!("{home}/.local/share/flatpak/exports/share"), "/var/lib/flatpak/exports/share".into()]);
    let mut out: Vec<std::path::PathBuf> = vec![];
    for d in dirs.iter().map(|d| std::path::Path::new(d).join("applications")).chain([std::path::PathBuf::from("/var/lib/snapd/desktop/applications")]) {
        if !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

/// Linux: .desktop files that declare they open https (`x-scheme-handler/https` in `MimeType`). Of the same file name, keep the one in the
/// earlier directory
fn desktop_browsers(dirs: &[std::path::PathBuf]) -> Vec<Found> {
    let mut out: Vec<Found> = vec![];
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "desktop")).collect();
        files.sort();
        for f in files {
            let id = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let Some((name, exec)) = std::fs::read_to_string(&f).ok().and_then(|t| parse_desktop(&t)) else { continue };
            if out.iter().any(|o| o.id == id) {
                continue;
            }
            out.push(Found { name, how: How::Cmd(exec_argv(&exec)), id });
        }
    }
    out
}

/// The `[Desktop Entry]` section of a .desktop: an application that opens https and isn't hidden → (name, Exec)
fn parse_desktop(text: &str) -> Option<(String, String)> {
    let (mut name, mut exec, mut https, mut hidden, mut app) = (None, None, false, false, true);
    let mut entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            entry = line == "[Desktop Entry]";
            continue;
        }
        let Some((k, v)) = line.split_once('=').filter(|_| entry) else { continue };
        let v = v.trim();
        match k.trim() {
            "Name" => name = Some(v.to_string()),
            "Exec" => exec = Some(v.to_string()),
            "MimeType" => https = v.split(';').any(|m| m.trim() == "x-scheme-handler/https"),
            "NoDisplay" | "Hidden" => hidden |= v == "true",
            "Type" => app = v == "Application",
            _ => {}
        }
    }
    (https && !hidden && app).then_some(())?;
    Some((name?, exec.filter(|e| !e.is_empty())?))
}

/// Split the Exec line into arguments (double quotes and backslashes per the spec); the link placeholders (%u %U %f %F) become `{url}`,
/// other % codes are dropped
fn exec_argv(exec: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![];
    let (mut cur, mut quoted, mut any) = (String::new(), false, false);
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '\\' if quoted => cur.extend(chars.next()),
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        args.push(cur);
    }
    args.into_iter()
        .filter(|a| !matches!(a.as_str(), "%i" | "%c" | "%k" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m"))
        .map(|a| ["%u", "%U", "%f", "%F"].iter().fold(a, |a, code| a.replace(code, "{url}")).replace("%%", "%"))
        .collect()
}

/// Linux's default browser (a .desktop file name): ask xdg-settings first; without it, read mimeapps.list
fn default_desktop() -> Option<String> {
    if let Ok(o) = Command::new("xdg-settings").args(["get", "default-web-browser"]).output() {
        let id = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if o.status.success() && id.ends_with(".desktop") {
            return Some(id);
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let config = std::env::var("XDG_CONFIG_HOME").ok().filter(|v| !v.is_empty()).unwrap_or(format!("{home}/.config"));
    let mut lists = vec![format!("{config}/mimeapps.list"), "/etc/xdg/mimeapps.list".into()];
    lists.extend(app_dirs().iter().map(|d| d.join("mimeapps.list").to_string_lossy().into_owned()));
    lists.iter().find_map(|f| std::fs::read_to_string(f).ok().and_then(|t| default_in_mimeapps(&t)))
}

/// Who handles https in mimeapps.list's `[Default Applications]`
fn default_in_mimeapps(text: &str) -> Option<String> {
    let mut on = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            on = line == "[Default Applications]";
        } else if let Some(v) = line.strip_prefix("x-scheme-handler/https=").filter(|_| on) {
            return v.split(';').map(str::trim).find(|s| !s.is_empty()).map(String::from);
        }
    }
    None
}

/// Put the link into the command: in place of `{url}` if present, otherwise at the end
fn fill(cmd: &[String], url: &str) -> Vec<String> {
    let mut argv: Vec<String> = cmd.iter().map(|a| a.replace("{url}", url)).collect();
    if !cmd.iter().any(|a| a.contains("{url}")) {
        argv.push(url.to_string());
    }
    argv
}

/// macOS's default browser (the bundle id that handles https); never set or unreadable = None
fn default_browser() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = std::env::var("HOME").ok()?;
    let ls = format!("{home}/Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist");
    let v: serde_json::Value = serde_json::from_str(&plutil(&["-convert", "json", "-o", "-", &ls])?).ok()?;
    v["LSHandlers"]
        .as_array()?
        .iter()
        .find(|h| h["LSHandlerURLScheme"] == "https")
        .and_then(|h| h["LSHandlerRoleAll"].as_str())
        .map(String::from)
}

fn plutil(args: &[&str]) -> Option<String> {
    let out = Command::new("/usr/bin/plutil").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

fn launch(how: &How, url: &str) -> Result<(), String> {
    let argv = match how {
        // Mac's open returns right away; wait for it to know whether it opened
        How::App(path) => return wait(Command::new("open").args(["-a", path, url])),
        How::System if cfg!(target_os = "macos") => return wait(Command::new("open").arg(url)),
        How::System => vec!["xdg-open".to_string(), url.to_string()],
        How::Cmd(cmd) => fill(cmd, url),
        How::Copy | How::CopyCode | How::Elsewhere => return Ok(()),
    };
    // A browser command may keep running (the first firefox on Linux does): don't wait, keep its output off the interface; reap it when it
    // exits
    let mut child = Command::new(aca_core::paths::expand(&argv[0]))
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| tr!("无法打开({}):{e}", "couldn't open it ({}): {e}", argv[0]))?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

fn wait(cmd: &mut Command) -> Result<(), String> {
    match cmd.output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(tr!("无法打开:{}", "couldn't open it: {}", String::from_utf8_lossy(&o.stderr).trim())),
        Err(e) => Err(tr!("无法打开:{e}", "couldn't open it: {e}")),
    }
}

/// The link is too long: keep only the host and path
fn short(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    match rest.split_once('?') {
        Some((head, _)) => format!("{head}?…"),
        None => rest.to_string(),
    }
}

/// Where a login link sends the browser back to, when that's this machine (`redirect_uri=http://localhost:1455/auth/callback`):
/// the login is waiting there. None = it sends it elsewhere, or doesn't say
fn callback_of(url: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    let raw = query.split('&').find_map(|kv| kv.strip_prefix("redirect_uri="))?;
    let back = unescape(raw);
    let rest = back.strip_prefix("http://")?;
    let host = rest.split('/').next()?;
    let port = host.strip_prefix("localhost:").or_else(|| host.strip_prefix("127.0.0.1:"))?;
    port.parse::<u16>().ok().map(|_| back.clone())
}

/// A URL's `state` (what ties a login's link to the address its browser comes back to)
fn state_of(url: &str) -> Option<&str> {
    url.split_once('?')?.1.split('&').find_map(|kv| kv.strip_prefix("state=")).map(str::trim)
}

/// %XX in a URL's query back to the bytes they stand for (`+` is a space)
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let pair = (b[i] == b'%' && i + 2 < b.len()).then(|| Some(hex(b[i + 1])? * 16 + hex(b[i + 2])?)).flatten();
        match (pair, b[i]) {
            (Some(v), _) => {
                out.push(v);
                i += 3;
                continue;
            }
            (None, b'+') => out.push(b' '),
            (None, c) => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The address the browser on the other computer landed on, pasted back: make the same request it would have made to the login
/// waiting here at `back` (same path; localhost and 127.0.0.1 count as one). Only for this machine's own address, with a code in it
fn deliver(back: &str, pasted: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    let pasted = pasted.trim();
    // Copied from an address bar that hides the scheme
    let pasted = if pasted.starts_with("http") { pasted.to_string() } else { format!("http://{pasted}") };
    let same = |u: &str| u.replacen("://127.0.0.1:", "://localhost:", 1);
    let want = || tr!("要粘贴 {back}?… 这样的地址", "paste an address like {back}?…");
    let query = same(&pasted).strip_prefix(&same(back)).and_then(|r| r.strip_prefix('?')).map(String::from).ok_or_else(want)?;
    if !query.split('&').any(|kv| kv.starts_with("code=")) {
        return Err(t!("地址里没有 code:网页上的登录还没完成", "there's no code in it: the login on the web page isn't done").into());
    }
    let rest = back.split_once("://").map_or(back, |(_, r)| r);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let port: u16 = host.rsplit_once(':').and_then(|(_, p)| p.parse().ok()).ok_or_else(want)?;
    let gone = || {
        t!("没有登录在等:登录终端里已经结束了,重新登录再试", "no login is waiting: it ended in the login terminal; log in again")
            .to_string()
    };
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut c = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(3)).map_err(|_| gone())?;
    let _ = c.set_read_timeout(Some(std::time::Duration::from_secs(15)));
    c.write_all(format!("GET /{path}?{query} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes()).map_err(|_| gone())?;
    // Only the status line matters
    let mut got = Vec::new();
    let mut buf = [0u8; 1024];
    while !got.windows(2).any(|w| w == b"\r\n") && got.len() < 8192 {
        match c.read(&mut buf) {
            Ok(n @ 1..) => got.extend_from_slice(&buf[..n]),
            _ => break,
        }
    }
    let text = String::from_utf8_lossy(&got);
    let line = text.lines().next().unwrap_or_default().trim().to_string();
    match line.split_whitespace().nth(1).and_then(|s| s.parse::<u16>().ok()) {
        Some(200..=399) => Ok(()),
        Some(_) => Err(tr!("登录没有收下:{line}", "the login didn't take it: {line}")),
        None => Err(gone()),
    }
}

impl Overlay for Pick {
    fn width(&self) -> u16 {
        64
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        if let Some((text, why)) = &self.paste {
            let title = t!("在别的电脑登录", "Log in on another computer");
            let mut out = vec![kit::modal_title(title, theme::accent(), kit::esc_hint(t!("返回", "back")), w), Line::raw("")];
            out.push(Line::styled(kit::clip(&self.title, w).trim_end().to_string(), kit::dim()));
            let said = t!(
                "已复制链接 · 登录后，把浏览器跳到的地址粘贴到这里",
                "link copied · after logging in, paste the address the browser went to"
            );
            out.extend(crate::setup::fold(said, w).into_iter().map(Line::raw));
            out.push(Line::raw(""));
            // A long address shows its end, where the typing is
            let shown = kit::clip_left(text, w.saturating_sub(3));
            out.push(Line::from(vec![Span::styled("> ", kit::key_style()), Span::raw(shown), Span::styled("▏", kit::key_style())]));
            if !why.is_empty() {
                out.extend(crate::setup::fold(why, w).into_iter().map(|l| Line::styled(l, theme::warn())));
            }
            out.push(Line::raw(""));
            out.push(kit::keys_line(&[("⏎", t!("送回登录", "hand it to the login"))], vec![], w));
            return out;
        }
        let title = if self.far {
            t!("在手机或其他电脑上登录", "Log in on your phone / another computer")
        } else {
            t!("如何打开登录链接?", "How to open the login link?")
        };
        let mut out = vec![kit::modal_title(title, theme::accent(), kit::esc_hint(t!("暂不打开", "not now")), w), Line::raw("")];
        out.push(Line::styled(kit::clip(&self.title, w).trim_end().to_string(), kit::dim()));
        out.push(Line::styled(kit::clip(&short(&self.link.url), w).trim_end().to_string(), kit::dim()));
        if let Some(code) = &self.link.code {
            out.push(Line::from(vec![Span::raw(t!("验证码  ", "one-time code  ")), Span::styled(code.clone(), kit::key_style())]));
        }
        if let Some(h) = self.hint() {
            out.extend(crate::setup::fold(h, w).into_iter().map(|l| Line::styled(l, kit::dim())));
        }
        out.push(Line::raw(""));
        for (i, it) in self.items.iter().enumerate() {
            let key = if i < 9 { (i + 1).to_string() } else { " ".into() };
            let line = Line::from(vec![
                Span::styled(key, kit::key_style()),
                Span::raw("  "),
                Span::raw(kit::clip(&it.label, w.saturating_sub(3)).trim_end().to_string()),
            ]);
            out.push(kit::picked(line, w, i == self.sel));
        }
        out.push(Line::raw(""));
        out.push(kit::keys_line(&[("⏎", t!("选择", "select"))], vec![], w));
        out
    }

    fn typing(&self) -> bool {
        self.paste.is_some()
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        if let (Some((text, why)), Some(back)) = (self.paste.as_mut(), self.back.as_deref()) {
            let ctrl = k.modifiers.contains(ratatui::crossterm::event::KeyModifiers::CONTROL);
            match k.code {
                // Back to the list: the link is still copied, the login still waiting
                KeyCode::Esc => self.paste = None,
                // Another login's address (an older link opened over there): the login here would only answer 400
                KeyCode::Enter if matches!((state_of(&self.link.url), state_of(text)), (Some(a), Some(b)) if a != b) => {
                    *why = t!(
                        "不是这次登录的地址:用这次复制的链接再登录一遍",
                        "not this login's address: log in again with the link copied this time"
                    )
                    .into();
                }
                KeyCode::Enter => match (self.send)(back, text) {
                    Ok(()) => {
                        fx.say(t!("已送回登录 · 结果看登录终端", "handed to the login · see the login terminal"));
                        return Flow::Close;
                    }
                    Err(e) => *why = e,
                },
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char('u') if ctrl => text.clear(),
                // A pasted address arrives as keys; line breaks around it aren't part of it
                KeyCode::Char(c) if !ctrl && !c.is_control() => text.push(c),
                _ => {}
            }
            return Flow::Stay;
        }
        let n = self.items.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                fx.say(t!("未打开 · 链接仍在登录终端中", "not opened · the link is still in the login terminal"));
                Flow::Close
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.sel = (self.sel + 1) % n;
                Flow::Stay
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.sel = (self.sel + n - 1) % n;
                Flow::Stay
            }
            KeyCode::Enter => self.choose(self.sel, fx),
            KeyCode::Char('c') => self.choose(0, fx),
            KeyCode::Char(d @ '1'..='9') => self.choose(d as usize - '1' as usize, fx),
            _ => Flow::Stay,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    const URL: &str = "https://claude.com/cai/oauth/authorize?code=true&client_id=x&state=y";

    fn found() -> Vec<Found> {
        [("Safari", "com.apple.Safari"), ("Google Chrome", "com.google.Chrome"), ("Zen Browser", "app.zen-browser.zen")]
            .iter()
            .map(|(n, id)| Found { name: n.to_string(), how: How::App(format!("/Applications/{n}.app")), id: id.to_string() })
            .collect()
    }

    /// Opening Zen fails, everything else opens
    fn fake(how: &How, _: &str) -> Result<(), String> {
        match how {
            How::App(p) if p.contains("Zen") => Err("无法打开:Zen 坏了".into()),
            _ => Ok(()),
        }
    }

    fn link(url: &str, code: Option<&str>) -> Link {
        Link { url: url.into(), code: code.map(String::from) }
    }

    fn pick() -> Pick {
        Pick::with("登录 Claude Code · ding", link(URL, None), false, arrange(found(), Some("com.google.chrome".into())), fake)
    }

    fn press(p: &mut Pick, code: KeyCode) -> (Flow, Fx) {
        let mut fx = Fx::default();
        let f = p.on_key(KeyEvent::from(code), &mut fx);
        (f, fx)
    }

    fn text(p: &Pick, w: usize) -> Vec<String> {
        p.lines(w).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    fn shows(rows: &[String], want: &[&str]) {
        for w in want {
            assert!(rows.iter().any(|r| r.contains(w)), "缺「{w}」:{rows:#?}");
        }
    }

    #[test]
    fn copying_comes_first_then_the_browsers_with_the_default_one_on_top() {
        let p = pick();
        let rows = text(&p, 60);
        shows(
            &rows,
            &[
                "如何打开登录链接?",
                "登录 Claude Code · ding",
                "claude.com/cai/oauth/authorize?…",
                "1  复制链接",
                "2  Google Chrome(默认)",
                "3  Safari",
                "4  Zen Browser",
            ],
        );
        assert_eq!(p.sel, 0);
        // The link's one-time parameters aren't shown; locally there's nothing to say about finishing (the callback comes to this machine)
        assert!(rows.iter().all(|r| !r.contains("state=y") && !r.contains("贴回")));
        // Can't tell what's installed: the system default
        let bare: Vec<String> = arrange(vec![], None).into_iter().map(|i| i.label).collect();
        assert_eq!(bare, ["默认浏览器"]);
        // The default isn't among the recognized ones: original order, unmarked
        let none: Vec<String> = arrange(found(), Some("org.mozilla.firefox".into())).into_iter().map(|i| i.label).collect();
        assert_eq!(none[..2], ["Safari", "Google Chrome"]);
        // Nothing wider than a phone
        assert!(text(&p, 40).iter().all(|r| r.width() <= 40), "{:#?}", text(&p, 40));
    }

    #[test]
    fn enter_copies_by_default_a_number_opens_and_esc_leaves_the_link_waiting() {
        // ⏎ right away: copy
        let (f, fx) = press(&mut pick(), KeyCode::Enter);
        assert_eq!((f, fx.copy.as_deref()), (Flow::Close, Some(URL)));
        let mut p = pick();
        let (f, fx) = press(&mut p, KeyCode::Char('3'));
        assert_eq!((f, fx.flash.as_deref()), (Flow::Close, Some("已在 Safari 打开 · 登录后返回终端")));
        // Can't open: say why, the list stays
        let (f, fx) = press(&mut p, KeyCode::Char('4'));
        assert_eq!((f, fx.flash.as_deref()), (Flow::Stay, Some("无法打开:Zen 坏了")));
        // ↓ then ⏎ = the second one
        let mut p = pick();
        press(&mut p, KeyCode::Down);
        assert_eq!(press(&mut p, KeyCode::Enter).1.flash.as_deref(), Some("已在 Google Chrome(默认) 打开 · 登录后返回终端"));
        assert_eq!(press(&mut pick(), KeyCode::Char('c')).1.copy.as_deref(), Some(URL));
        // A number with no item: nothing happens
        assert_eq!(press(&mut pick(), KeyCode::Char('9')).0, Flow::Stay);
        let (f, fx) = press(&mut pick(), KeyCode::Esc);
        assert!(f == Flow::Close && fx.flash.is_some_and(|m| m.contains("链接仍在登录终端中")) && fx.copy.is_none());
    }

    #[test]
    fn far_away_there_are_no_browsers_and_it_says_how_to_finish() {
        // Claude remote: a link that needs the code pasted back, copy only
        let p = Pick::with("登录 Claude Code · ding", link(URL, None), true, vec![], fake);
        let rows = text(&p, 60);
        shows(&rows, &["在手机或其他电脑上登录", "1  复制链接", "将网页上的验证码粘贴到下方终端"]);
        assert_eq!(p.items.len(), 1);
        // Codex device code: the code is shown in the list, link and code are both copyable, the list stays after copying one
        let mut p = Pick::with("登录 Codex · px", link("https://auth.openai.com/codex/device", Some("AB12-CD3EF")), true, vec![], fake);
        let rows = text(&p, 60);
        shows(&rows, &["auth.openai.com/codex/device", "验证码  AB12-CD3EF", "1  复制链接", "2  复制验证码 AB12-CD3EF", "15 分钟内有效"]);
        let (f, fx) = press(&mut p, KeyCode::Char('2'));
        assert_eq!((f, fx.copy.as_deref()), (Flow::Stay, Some("AB12-CD3EF")));
        let (f, fx) = press(&mut p, KeyCode::Enter);
        assert_eq!((f, fx.copy.as_deref()), (Flow::Stay, Some("https://auth.openai.com/codex/device")));
        assert!(text(&p, 40).iter().all(|r| r.width() <= 40), "{:#?}", text(&p, 40));
    }

    #[test]
    fn linux_finds_browsers_in_desktop_files_and_the_default_from_mimeapps() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // Exec: quotes, % codes; the link placeholders become {url}, other % codes are dropped
        assert_eq!(exec_argv("/usr/lib/firefox/firefox %u"), s(&["/usr/lib/firefox/firefox", "{url}"]));
        assert_eq!(exec_argv("\"/opt/my browser/run\" --new-window %U %i"), s(&["/opt/my browser/run", "--new-window", "{url}"]));
        assert_eq!(exec_argv("/usr/bin/flatpak run --command=zen app.zen_browser.zen @@u %u @@"), s(&["/usr/bin/flatpak", "run", "--command=zen", "app.zen_browser.zen", "@@u", "{url}", "@@"]));
        assert_eq!(exec_argv("chromium --app=\"x\\\"y\" 100%%"), s(&["chromium", "--app=x\"y", "100%"]));
        assert_eq!(fill(&s(&["firefox", "{url}"]), "https://x"), s(&["firefox", "https://x"]));
        assert_eq!(fill(&s(&["google-chrome", "--profile-directory=Profile 1"]), "https://x"), s(&["google-chrome", "--profile-directory=Profile 1", "https://x"]));

        let root = std::env::temp_dir().join(format!("aca-desktop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, sys) = (root.join("user"), root.join("sys"));
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&sys).unwrap();
        let entry = |name: &str, exec: &str, extra: &str| {
            format!("[Desktop Entry]\nType=Application\nName={name}\nName[zh_CN]=别的名字\nExec={exec}\n{extra}\n[Desktop Action new-window]\nName=New Window\nExec=nope\n")
        };
        let https = "MimeType=text/html;x-scheme-handler/http;x-scheme-handler/https;";
        std::fs::write(sys.join("firefox.desktop"), entry("Firefox", "firefox %u", https)).unwrap();
        std::fs::write(sys.join("google-chrome.desktop"), entry("Google Chrome", "/usr/bin/google-chrome-stable %U", https)).unwrap();
        // Not a browser, or hidden: not listed
        std::fs::write(sys.join("gimp.desktop"), entry("GIMP", "gimp %U", "MimeType=image/png;")).unwrap();
        std::fs::write(sys.join("helper.desktop"), entry("Helper", "helper %u", &format!("{https}\nNoDisplay=true"))).unwrap();
        std::fs::write(sys.join("firefox-esr.desktop"), entry("Firefox", "firefox-esr %u", https)).unwrap();
        // The same file name in the user's own directory overrides the system's
        std::fs::write(user.join("google-chrome.desktop"), entry("Google Chrome", "/usr/bin/google-chrome-stable --profile-directory=Work %U", https)).unwrap();
        let found = desktop_browsers(&[user.clone(), sys.clone(), root.join("没有这个目录")]);
        let got: Vec<(&str, &str)> = found.iter().map(|f| (f.name.as_str(), f.id.as_str())).collect();
        assert_eq!(got, [("Google Chrome", "google-chrome.desktop"), ("Firefox", "firefox-esr.desktop"), ("Firefox", "firefox.desktop")]);
        assert_eq!(found[0].how, How::Cmd(s(&["/usr/bin/google-chrome-stable", "--profile-directory=Work", "{url}"])));
        // The default is read from mimeapps.list, goes first, marked; of same-named ones only the default stays
        let mime = "[Added Associations]\nx-scheme-handler/https=google-chrome.desktop;\n[Default Applications]\nx-scheme-handler/http=firefox.desktop\nx-scheme-handler/https=firefox.desktop;google-chrome.desktop;\n";
        assert_eq!(default_in_mimeapps(mime).as_deref(), Some("firefox.desktop"));
        assert_eq!(default_in_mimeapps("[Default Applications]\ntext/html=x.desktop\n"), None);
        let items = arrange(found, default_in_mimeapps(mime));
        assert_eq!(items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["Firefox(默认)", "Google Chrome"]);
        assert_eq!(items[0].how, How::Cmd(s(&["firefox", "{url}"])));
        std::fs::remove_dir_all(&root).unwrap();
    }

    const CODEX: &str = "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_x\
                         &redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&scope=openid&state=s1";

    /// What the login took: the last address handed over (one per test thread)
    fn taken(back: &str, pasted: &str) -> Result<(), String> {
        if pasted.contains("code=") { Ok(()) } else { Err(format!("{back} 没收下 {pasted}")) }
    }

    #[test]
    fn a_login_waiting_here_can_be_finished_on_another_computer_by_pasting_where_its_browser_went() {
        assert_eq!(callback_of(CODEX).as_deref(), Some("http://localhost:1455/auth/callback"));
        // Sent elsewhere (Claude's paste-a-code page), or not said: nothing to paste back
        assert_eq!(callback_of("https://claude.com/x?redirect_uri=https%3A%2F%2Fplatform.claude.com%2Fcallback"), None);
        assert_eq!(callback_of(URL), None);
        assert_eq!(unescape("a%2Fb+c%zz%4"), "a/b c%zz%4");

        let mut p = Pick::with("登录 Codex · team-unis", link(CODEX, None), false, arrange(found(), None), fake);
        p.send = taken;
        shows(&text(&p, 60), &["1  复制链接", "2  在别的电脑登录", "3  Safari"]);
        // Picked: the link is copied, then the address is taken
        let (f, fx) = press(&mut p, KeyCode::Char('2'));
        assert_eq!((f, fx.copy.as_deref()), (Flow::Stay, Some(CODEX)));
        assert!(p.typing());
        let rows = text(&p, 60);
        shows(&rows, &["在别的电脑登录", "Esc 返回", "登录 Codex · team-unis", "已复制链接", "> ▏", "⏎ 送回登录"]);
        // Pasted as keys; not taken: why, it stays
        for c in "localhost:1455/auth/callback?state=s1".chars() {
            press(&mut p, KeyCode::Char(c));
        }
        assert_eq!(press(&mut p, KeyCode::Enter).0, Flow::Stay);
        shows(&text(&p, 60), &["没收下"]);
        // ^U clears; another login's address (its state differs): said so before anything is sent
        let ctrl_u = || KeyEvent::new(KeyCode::Char('u'), ratatui::crossterm::event::KeyModifiers::CONTROL);
        p.on_key(ctrl_u(), &mut Fx::default());
        for c in "localhost:1455/auth/callback?code=ab&state=old".chars() {
            press(&mut p, KeyCode::Char(c));
        }
        assert_eq!(press(&mut p, KeyCode::Enter).0, Flow::Stay);
        shows(&text(&p, 60), &["不是这次登录的地址"]);
        // Taken: closes, saying where to look
        p.on_key(ctrl_u(), &mut Fx::default());
        assert_eq!(p.paste.as_ref().map(|(t, _)| t.as_str()), Some(""));
        for c in "http://localhost:1455/auth/callback?code=ab&state=s1".chars() {
            press(&mut p, KeyCode::Char(c));
        }
        let (f, fx) = press(&mut p, KeyCode::Enter);
        assert_eq!((f, fx.flash.as_deref()), (Flow::Close, Some("已送回登录 · 结果看登录终端")));
        // Esc while pasting: back to the list, not closed
        let mut p = Pick::with("登录 Codex · team-unis", link(CODEX, None), false, vec![], fake);
        press(&mut p, KeyCode::Char('2'));
        assert_eq!(press(&mut p, KeyCode::Esc).0, Flow::Stay);
        assert!(!p.typing() && text(&p, 60).iter().any(|r| r.contains("如何打开登录链接")));
        // A device code login needs nothing pasted back
        let p = Pick::with("登录 Codex · x", link(CODEX, Some("AB12-CD3EF")), true, vec![], fake);
        assert!(!text(&p, 60).iter().any(|r| r.contains("在别的电脑登录")));
    }

    #[test]
    fn the_pasted_address_goes_to_the_login_waiting_here_as_the_browser_would_have_sent_it() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 512];
            while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = c.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
            }
            c.write_all(b"HTTP/1.1 302 Found\r\nLocation: /success\r\n\r\n").unwrap();
            String::from_utf8_lossy(&got).lines().next().unwrap().to_string()
        });
        let back = format!("http://localhost:{port}/auth/callback");
        // 127.0.0.1 or localhost alike, the scheme may be missing
        deliver(&back, &format!("  127.0.0.1:{port}/auth/callback?code=ab&state=s1\n")).unwrap();
        assert_eq!(server.join().unwrap(), "GET /auth/callback?code=ab&state=s1 HTTP/1.1");
        // Not this login's address, no code: said so, nothing sent
        assert!(deliver(&back, "http://example.com/auth/callback?code=ab").unwrap_err().contains(&back));
        assert!(deliver(&back, &format!("{back}?error=access_denied")).unwrap_err().contains("code"));
        // Nobody waiting any more
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let gone = deliver(&format!("http://localhost:{free}/auth/callback"), &format!("localhost:{free}/auth/callback?code=x"));
        assert!(gone.unwrap_err().contains("没有登录在等"));
    }
}
