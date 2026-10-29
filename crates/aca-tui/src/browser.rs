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
        items.extend(browsers);
        Pick { title: title.to_string(), link, far, items, sel: 0, open }
    }

    fn choose(&self, i: usize, fx: &mut Fx) -> Flow {
        let Some(it) = self.items.get(i) else { return Flow::Stay };
        let copied = match &it.how {
            How::Copy => self.link.url.clone(),
            How::CopyCode => self.link.code.clone().unwrap_or_default(),
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
        How::Copy | How::CopyCode => return Ok(()),
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

impl Overlay for Pick {
    fn width(&self) -> u16 {
        64
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
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

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
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
}
