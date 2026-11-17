//! Running the interface: setting the terminal up and giving it back, the event loop, external programs it hands the screen
//! to, and `aca ui --snapshot`.

use super::*;

/// How long the startup quiet period is. by_hand = opened by your own keypress (in a tmux popup, ^L restart): no Moshi is connecting then,
/// the quiet period would only swallow the ⏎ pressed right after (keys pressed before the popup is drawn are read right after the first
/// frame; any quiet period, however short, swallows them)
pub(super) fn startup_quiet(by_hand: bool) -> Duration {
    if by_hand { Duration::ZERO } else { STARTUP_QUIET }
}

/// Whether to drop this batch of keys (why, see the run loop). quiet = still in the quiet period of startup / just leaving a session. Keys
/// that only move the cursor are never dropped: commands Moshi types and pastes have no arrow keys, while swiping or holding an arrow on a
/// phone sends them one after another — the few landing in the quiet period get dropped, those arriving together extend it, and the whole
/// run is lost
pub(super) fn drops(keys: &[KeyEvent], quiet: bool, typing: bool) -> bool {
    let nav = |k: &KeyEvent| {
        matches!(
            k.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
        )
    };
    !keys.is_empty() && !keys.iter().all(nav) && (quiet || (keys.len() >= 3 && !typing))
}

/// Keys dropped as automatic input are appended to ~/.cache/aca/ignored-input.log (last 200 lines only, mode 600). With ACA_TMUX_SOCKET set
/// it writes ignored-input.<it>.log, so tests don't mix into the real log
fn log_ignored(keys: &[KeyEvent]) {
    let text: String = keys
        .iter()
        .map(|k| match k.code {
            KeyCode::Char(c) if k.modifiers.contains(KeyModifiers::CONTROL) => format!("^{}", c.to_ascii_uppercase()),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "⏎".into(),
            KeyCode::Esc => "⎋".into(),
            KeyCode::Tab => "⇥".into(),
            other => format!("<{other:?}>"),
        })
        .collect();
    let Some(home) = std::env::var_os("HOME") else { return };
    let file = match std::env::var("ACA_TMUX_SOCKET") {
        Ok(s) if !s.is_empty() => format!("ignored-input.{s}.log"),
        _ => "ignored-input.log".into(),
    };
    let path = std::path::Path::new(&home).join(".cache/aca").join(file);
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<&str> = old.lines().collect();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let line = format!("{stamp} {text}");
    lines.push(&line);
    let keep = &lines[lines.len().saturating_sub(200)..];
    let _ = std::fs::write(&path, keep.join("\n") + "\n");
    // Anything pasted into the list by mistake gets logged here too: readable only by you
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
}

/// The terminal supports kitty's extended keyboard protocol (desktop kitty): only with it on can Shift+Enter be told from Enter. Phones go
/// through mosh, which doesn't forward this protocol; there only `E` works
static KBD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn kbd(on: bool) {
    if KBD.load(Ordering::Relaxed) {
        let _ = if on {
            execute!(stdout(), PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))
        } else {
            execute!(stdout(), PopKeyboardEnhancementFlags)
        };
    }
}

/// What the settings page lists under Problems: the theme (config.toml's [theme], files in themes/) and a few other items
fn config_bad() -> Vec<String> {
    [theme::problems(), crate::config::problems()].concat()
}

/// Focus events are needed only when managing the input method (desktop);
/// turned off before handing over the terminal, since programs inside don't know them
fn focus_events(on: bool) {
    if crate::ime::active() {
        let _ = if on { execute!(stdout(), EnableFocusChange) } else { execute!(stdout(), DisableFocusChange) };
    }
}

/// When opened directly in kitty: store the highlighted item in this window's variables (OSC 1337 SetUserVar): `aca_dir` its directory,
/// `aca_sid` which session it is. kitty's floating window (kfloat, run by cmd+enter) opens there and is tied to that session. Empty =
/// delete the variable (on exit)
fn tell_var(name: &str, value: &str) {
    use std::io::Write;
    let v = if value.is_empty() { String::new() } else { format!("={}", crate::clip::base64(value.as_bytes())) };
    let mut out = stdout().lock();
    let _ = write!(out, "\x1b]1337;SetUserVar={name}{v}\x07").and_then(|_| out.flush());
}

/// Whether a note can be filled into its input box (E takes it in, s sends it): only Claude / Codex (other programs like vim would take
/// pasted text as commands); not while it waits for you to choose (!) — the text would be taken as a choice
pub(super) fn cant_fill(s: &Session) -> Option<&'static str> {
    match s.state {
        State::Ask => Some(t!("正在等待回应", "it's waiting for your choice")),
        State::Error | State::Exited | State::Stopped | State::Crashed | State::Dead => Some(t!("未运行", "it isn't running")),
        _ if !matches!(s.agent, Agent::Claude | Agent::Codex | Agent::Pi | Agent::Opencode) => {
            Some(t!("不是内置 agent", "not a built-in agent"))
        }
        _ => None,
    }
}

/// l / e in the diff view, y in the list: hand over the terminal to run lazygit / an editor / yazi, return what went wrong when back
fn run_external(x: &External) -> Option<String> {
    let q = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let (name, st) = match x {
        External::Lazygit(dir) => ("lazygit".to_string(), std::process::Command::new("lazygit").arg("-p").arg(dir).status()),
        External::Edit { path, line } => {
            let ed = notes::editor();
            let name = notes::editor_name();
            // Each editor writes "open at line N" differently:
            // the VS Code family uses -g file:line, helix / zed / sublime use file:line, the rest +line file
            let cmd = match name.as_str() {
                "code" | "code-insiders" | "cursor" | "windsurf" => format!("{ed} -g {}", q(&format!("{path}:{line}"))),
                "hx" | "helix" | "zed" | "subl" => format!("{ed} {}", q(&format!("{path}:{line}"))),
                _ => format!("{ed} +{line} {}", q(path)),
            };
            (name, std::process::Command::new("sh").arg("-c").arg(cmd).status())
        }
        External::Config { path, template } => {
            let p = std::path::Path::new(path);
            if !p.exists() {
                let made = p.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(p, template));
                if let Err(e) = made {
                    return Some(tr!("无法写入 {}:{e}", "can't write {}: {e}", words::tilde(path)));
                }
            }
            let cmd = format!("{} {}", notes::editor(), q(path));
            (notes::editor_name(), std::process::Command::new("sh").arg("-c").arg(cmd).status())
        }
        External::View { path, line, temp } => {
            let ed = notes::editor();
            let name = notes::editor_name();
            // Read-only: vim family -M (can't edit or save), no swap file, no prompt if open elsewhere; nano -v; other editors have no
            // read-only mode, open them as usual
            let gui = matches!(name.as_str(), "code" | "code-insiders" | "cursor" | "windsurf" | "zed" | "subl");
            let cmd = match name.as_str() {
                "vi" | "vim" | "nvim" | "view" | "gvim" | "mvim" => format!("{ed} -M -n --cmd 'set shortmess+=A' +{line} {}", q(path)),
                "nano" => format!("{ed} -v +{line} {}", q(path)),
                "code" | "code-insiders" | "cursor" | "windsurf" => format!("{ed} -g {}", q(&format!("{path}:{line}"))),
                "hx" | "helix" | "zed" | "subl" => format!("{ed} {}", q(&format!("{path}:{line}"))),
                _ => format!("{ed} +{line} {}", q(path)),
            };
            let st = std::process::Command::new("sh").arg("-c").arg(cmd).status();
            // Editors that open a window return at once; the file stays (files older than a day are cleared next time)
            if *temp && !gui {
                let _ = std::fs::remove_file(path);
            }
            (name, st)
        }
        External::Yazi(sp) => {
            let dir = aca_records::workdir::find(&sp.w).dir;
            if let Some(e) = yazi_blocked(&dir) {
                return Some(e);
            }
            ("yazi".to_string(), std::process::Command::new("yazi").arg(&dir).status())
        }
        External::Plugin(req) => {
            let (ok, out) = crate::caps::run(req);
            return (!ok).then_some(out);
        }
    };
    match st {
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(not_found(&name)),
        Err(e) => Some(tr!("无法启动 {name}:{e}", "can't start {name}: {e}")),
    }
}

/// How to install a missing program: brew on macOS; elsewhere (Linux mostly has no brew) use your own package manager
fn not_found(name: &str) -> String {
    if cfg!(target_os = "macos") {
        tr!("未找到 {name}(brew install {name})", "{name} not found (brew install {name})")
    } else {
        tr!("未找到 {name}:请用包管理器安装", "{name} not found: install it with your package manager")
    }
}

/// Write the text for e to a temp file: in a directory only you can enter, named like "turn 3-Bash" (shown at the top of the editor); also
/// clear leftovers older than a day
pub(super) fn write_view(name: &str, text: &str, ext: &str) -> std::io::Result<String> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = std::env::temp_dir().join("aca-trace-view");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let old = |e: &std::fs::DirEntry| {
            e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|d| d > Duration::from_secs(86_400)))
        };
        rd.flatten().filter(old).for_each(|e| {
            let _ = std::fs::remove_file(e.path());
        });
    }
    let base: String = name.chars().filter(|c| !c.is_whitespace()).map(|c| if matches!(c, '/' | '·' | ':') { '-' } else { c }).collect();
    let path = dir.join(format!("{}.{ext}", if base.is_empty() { t!("记录", "record") } else { &base }));
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path)?;
    std::io::Write::write_all(&mut f, text.as_bytes())?;
    Ok(path.to_string_lossy().into_owned())
}

/// Wait for a key, up to max. With a terminal open, its output also wakes it to redraw
fn wait_input(max: Duration, term: bool) -> std::io::Result<bool> {
    if !term {
        return event::poll(max);
    }
    let end = Instant::now() + max;
    loop {
        if event::poll(Duration::from_millis(8))? {
            return Ok(true);
        }
        if crate::term::DIRTY.swap(false, Ordering::Relaxed) || Instant::now() >= end {
            return Ok(false);
        }
    }
}

/// Why yazi can't open: directory gone, not installed
fn yazi_blocked(dir: &str) -> Option<String> {
    if !std::path::Path::new(dir).is_dir() {
        return Some(if dir.is_empty() {
            t!("无法确定会话目录", "no idea which directory it's in").into()
        } else {
            tr!("目录已不存在:{dir}", "the directory is gone: {dir}")
        });
    }
    let found = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("yazi").is_file()));
    (!found).then(|| not_found("yazi"))
}

/// Give the terminal back on error (exitlog's cleanup calls this on the main thread): like a normal exit, turn off keyboard enhancement,
/// focus events and mouse, leave full screen and raw mode. Don't touch the input method: its lock may be held by the code that failed
/// (ime::Guard switches back on exit)
fn release_terminal() -> std::io::Result<()> {
    kbd(false);
    let _ = execute!(stdout(), DisableFocusChange, DisableMouseCapture);
    ratatui::try_restore()
}

/// Hand over the terminal (go into a session / fzf / editor). Turn off the extended keyboard protocol first, else programs inside get key
/// codes they don't know
fn suspend() -> std::io::Result<()> {
    kbd(false);
    focus_events(false);
    crate::ime::english(false);
    execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen)?;
    disable_raw_mode()
}

/// Take the terminal back and draw a frame right away (with the data at hand: what needs rereading goes to the background and is swapped in
/// when read). First send an "end sync": tmux wraps each frame in sync begin / end; if a detach lands mid-frame (the session is flooding
/// output), the unsent second half of the frame and its "end" are thrown away, kitty stays "in sync" and doesn't refresh, aca's drawn frame
/// stays invisible, stuck up to 2 seconds (measured: 7 of 100 detaches in a flooding session). A terminal not stuck in sync does nothing
/// with it. Entering full screen, clearing and drawing this frame are wrapped in one sync too: the terminal swaps it in when done, with no
/// blank screen in between (it used to clear first and then read the list, blank for tens of milliseconds, seen as a flash). Clear with
/// resize, not clear: to put the cursor back, clear first asks the terminal where the cursor is and waits for the answer; on a phone over
/// ssh, one stall (no answer for 2 seconds) is an error and aca exits with it. resize clears too and the next frame redraws the whole
/// screen, but it doesn't ask
fn resume(t: &mut DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EndSynchronizedUpdate, BeginSynchronizedUpdate, EnterAlternateScreen, EnableMouseCapture)?;
    kbd(true);
    focus_events(true);
    t.hide_cursor()?;
    let area = t.size()?.into();
    t.resize(area)?;
    t.draw(|f| draw(f, app))?;
    execute!(stdout(), EndSynchronizedUpdate)
}

/// Whether it's Ctrl+\. The terminal sends 0x1C, which crossterm reads as ^4; in terminals with keyboard enhancement it's ^\ itself
/// Whether a key pressed is this one (Shift isn't compared: a terminal reports B as b with or without it)
pub(super) fn key_is(k: KeyEvent, (code, mods): (KeyCode, KeyModifiers)) -> bool {
    let m = KeyModifiers::CONTROL | KeyModifiers::ALT;
    k.code == code && (k.modifiers & m) == (mods & m)
}

/// A tmux key name as the list receives it: `C-b`, `M-x`, `C-M-y`, `F6`, `b`; one it can't tell = None. `prefix b` is two keys:
/// tmux's prefix key (`prefix`, its name in tmux), then b
pub fn list_keys(key: &str, prefix: &str) -> Vec<(KeyCode, KeyModifiers)> {
    fn one(k: &str) -> Option<(KeyCode, KeyModifiers)> {
        if let Some(n) = k.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()).filter(|n| (1..=12).contains(n)) {
            return Some((KeyCode::F(n), KeyModifiers::NONE));
        }
        let (mut rest, mut mods) = (k, KeyModifiers::NONE);
        while rest.chars().count() > 1 {
            let (m, r) = match rest.split_at_checked(2)? {
                ("C-", r) => (KeyModifiers::CONTROL, r),
                ("M-", r) => (KeyModifiers::ALT, r),
                ("S-", r) => (KeyModifiers::SHIFT, r),
                _ => return None,
            };
            (mods, rest) = (mods | m, r);
        }
        let c = rest.chars().next()?;
        Some((KeyCode::Char(if mods.contains(KeyModifiers::CONTROL) { c.to_ascii_lowercase() } else { c }), mods))
    }
    let keys = match key.strip_prefix(aca_core::tmux::PREFIX_KEY) {
        Some(after) => vec![one(prefix.trim()), one(after.trim())],
        None => vec![one(key)],
    };
    keys.into_iter().collect::<Option<Vec<_>>>().unwrap_or_default()
}

pub(super) fn closes_popup(k: KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('\\' | '4'))
}

/// Replace this process with itself (same path, i.e. the newly installed one) and keep going: exec, same process, same terminal. Only pass
/// --client (the popup needs it), --insert (still picking a reference) and --restarted (where to open); not the keywords from startup — they'd go straight into that session. The
/// view (groups, scope, layout, highlighted session) is saved anyway, and the new one restores it
fn relaunch(state: &str) -> anyhow::Error {
    use std::os::unix::process::CommandExt;
    let Some(exe) = aca_core::agent::self_path() else {
        return anyhow::anyhow!("{}", t!("找不到 aca 程序,未重启", "can't find the aca program; not restarted"));
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("ui");
    if let Some(c) = crate::data::client() {
        cmd.args(["--client", c]);
    }
    if crate::follow::asked() {
        cmd.arg("--follow");
    }
    if let Some(p) = crate::data::insert_to() {
        cmd.args(["--insert", p]);
    }
    cmd.args(["--restarted", state]);
    anyhow::anyhow!("{}", tr!("重启界面失败:{}", "restarting the interface failed: {}", cmd.exec()))
}

/// When this aca program was installed (the file's mtime): 11:28 for today, 9/29 11:28 for other days
fn installed_at() -> Option<String> {
    let secs = |t: std::time::SystemTime| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64);
    let t = secs(std::fs::metadata(aca_core::agent::self_path()?).ok()?.modified().ok()?)?;
    let now = secs(std::time::SystemTime::now())?;
    Some(local_when(t.max(0) as u64, now.max(0) as u64))
}

/// Stop idle sessions on the timer from config.toml's `[idle] stop_after` (see `session::stop_idle`), sending the stopped ones back to the
/// interface. The config is read fresh each time, so changes need no reopen; missing or invalid (listed in the settings page) = no
/// stopping. Ones marked unread with u are kept: core's "seen" doesn't know about that
fn idle_watch(tx: mpsc::Sender<IdleTick>) {
    let mut wait = IDLE_FIRST;
    loop {
        thread::sleep(wait);
        wait = IDLE_EVERY;
        let cfg = aca_core::config::load();
        let Ok(store) = aca_core::store::Store::open() else { continue };
        let stopped = match cfg.idle.stop_after() {
            Ok(Some(after)) => {
                let reads = Reads::open(Reads::default_path());
                aca_core::session::stop_idle(&store, &cfg, after, &|id| reads.unread(id)).unwrap_or_default()
            }
            _ => vec![],
        };
        let remove = cfg.idle.remove_after().ok().flatten().map(|after| (after, archived_sessions(&store)));
        if tx.send(IdleTick { stopped, remove }).is_err() {
            return;
        }
    }
}

/// What `idle_watch` hands back each round: which ones were auto-stopped; if `[idle] remove_after` is set, it and the archived sessions
/// (see `App::sweep`)
struct IdleTick {
    stopped: Vec<aca_core::session::Idled>,
    remove: Option<(u64, Vec<(Target, u64)>)>,
}

/// Archived registered sessions (not listed) and their last activity: when archived, the hook's last report, the latest in the registry
fn archived_sessions(store: &aca_core::store::Store) -> Vec<(Target, u64)> {
    let all = store.sessions(true).unwrap_or_default();
    all.into_iter()
        .filter(|s| s.archived > 0)
        .map(|s| {
            let r = aca_core::runtime::load(&s.id);
            let title = aca_core::status::title(&s, &r);
            let at = s.archived.max(r.ts).max(s.created);
            (Target { name: s.id.clone(), title, profile: crate::data::REGISTERED.into(), id: s.id }, at)
        })
        .collect()
}

/// The bottom bar's line after auto-stopping some idle sessions: which ones (only the first three if many), how to start them again
pub(super) fn idle_note(v: &[aca_core::session::Idled]) -> String {
    let mut names: Vec<String> = v.iter().take(3).map(|s| tr!("「{}」", "“{}”", clip(&s.title, 16).trim_end())).collect();
    if v.len() > 3 {
        names.push(tr!("等 {} 个", "and {} more", v.len() - 3));
    }
    tr!(
        "{} 自动停止了 {} 个:{} · ⏎ 重新启动",
        "{} auto-stopped {}: {} · ⏎ resumes",
        aca_core::status::IDLE_STOPPED,
        v.len(),
        names.join(t!("", ", "))
    )
}

pub fn run(filter: String, restarted: Option<String>) -> anyhow::Result<()> {
    // Draw the first screen once read (~20ms), to avoid flashing an empty list first; read in the background while switching the input
    // method here (~40ms the first time)
    let first = thread::spawn(crate::data::collect_first);
    // Switch the input method first: keys pressed as soon as the popup appears don't get eaten by pinyin.
    // On the main thread, like later switches
    crate::ime::start();
    let _ime = crate::ime::Guard;
    // Opened with keywords: show only the matches, don't restore the group it was last on
    let view_file = aca_records::paths::state_file("view");
    let restore = filter.is_empty();
    let sessions = first.join().unwrap_or_else(|e| std::panic::resume_unwind(e));
    let mut app = App::new(filter, sessions)
        .with_pins(Pins::open(Pins::default_path()))
        .with_notes(Notes::open(Notes::default_path()))
        .with_marks(Reads::open(Reads::default_path()), Spaces::open(Spaces::default_path()))
        .with_trash(Trash::open(Trash::default_path()))
        .with_group_hints(crate::data::group_hints)
        .with_adopt(adopt_plan)
        .with_new_group_dir(crate::config::new_group_dir)
        .with_tools(|| Ok(crate::tools::load()))
        .with_accounts(|adding| Box::new(crate::acctpage::Page::real(adding)))
        .with_login_links(aca_core::paths::state_dir())
        .with_settings(crate::config::ime_auto, config_bad, crate::config::set, crate::accounts::all)
        .with_mcp(crate::mcp::load)
        .with_groups(crate::groups::load)
        .with_handoff(crate::handoff::source)
        .with_rehome(crate::rehome::quotas, crate::accounts::login, true);
    // The remote tasks server is set in the settings page; P is the plugins page (mounts and remote tasks are plugins now, in the : menu)
    app = app.with_plugins(|| Box::new(crate::plugins::Page::new(crate::plugins::load)));
    // Whether Claude starts with aca's mod (quota doesn't rely on the status bar)
    app = app.with_setting(Box::new(crate::modrow::ModRow::real()));
    app = app.with_setting(Box::new(crate::trust::TrustRow::real()));
    // Sessions a killed / crashed interface deleted but didn't get to really delete
    thread::spawn(crate::trash::purge);
    // Upgraded without running install.sh (brew and the like): rewrite aca's tmux settings for this version
    thread::spawn(crate::tmuxconf::refresh);
    if let (true, Some(p)) = (restore, &view_file) {
        app = app.restore(View::load(p));
    }
    if let Some((session, pane)) = crate::data::client_at() {
        app.highlight_here(&session, &pane);
        if crate::follow::asked() {
            // Closed while on another session, so `switch_to` didn't run: set up the Ctrl+Q binding here once
            aca_core::tmux::bind_detach(aca_core::plugin::hotkey_binds);
            app.follow_from(&session, &pane);
        }
    }
    app.insert_to = crate::data::insert_to().map(String::from);
    // The reference key works in the list too (no tmux in between: its prefix key arrives here as is)
    if let Ok(Some(k)) = aca_core::config::load().keys.ref_key() {
        let prefix = aca_core::tmux::run(&["show-options", "-gv", "prefix"]).unwrap_or_default();
        app.ref_keys = list_keys(&k, &prefix);
    }
    // config.toml is broken: treat accounts and tools as unset; the settings page's Problems says where
    app.sync_failed = aca_core::config::problem().into_iter().collect();
    if let Some(state) = &restarted {
        app.resume(state, installed_at());
    }
    // The last interface went away with its terminal / exited on error: say it once (see exitlog.rs)
    if let Some(m) = crate::exitlog::take_notice() {
        app.flash_for(&m, NOTICE);
    }
    let mut saved = app.view_state();
    let mut lang = i18n::lang();

    // The background rereads every 2 seconds; paused while attached
    let (tx, rx) = mpsc::channel::<Vec<Session>>();
    // Whether plugins have errors: reread along with the list (the ⚠ cell in the top bar)
    let (alert_tx, alert_rx) = mpsc::channel::<Vec<aca_core::plugin::health::Problem>>();
    let (req_tx, req_rx) = mpsc::channel::<()>();
    let paused = Arc::new(AtomicBool::new(false));
    {
        let paused = paused.clone();
        thread::spawn(move || {
            // Reread when it's time, when someone asks, or when a hook just reported (the event stream file changed); stop once the
            // interface is gone. However dense the events, refreshes they trigger stay EVENT_GAP apart
            let stamp = || std::fs::metadata(aca_core::paths::events_log()).and_then(|m| m.modified()).ok();
            let (mut seen, mut last) = (stamp(), Instant::now());
            loop {
                match req_rx.recv_timeout(EVENT_TICK) {
                    Ok(()) => {}
                    Err(RecvTimeoutError::Timeout) => {
                        let due = last.elapsed() >= REFRESH || (stamp() != seen && last.elapsed() >= EVENT_GAP);
                        if !due {
                            continue;
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                (seen, last) = (stamp(), Instant::now());
                if !paused.load(Ordering::Relaxed)
                    && (tx.send(collect()).is_err() || alert_tx.send(aca_core::plugin::health::problems()).is_err())
                {
                    break;
                }
            }
        });
    }

    // Stop idle sessions when their time is up: its own thread, not paused with the refresh —
    // it keeps watch whichever session you're attached to
    let (idle_tx, idle_rx) = mpsc::channel::<IdleTick>();
    thread::spawn(move || idle_watch(idle_tx));

    // Restart / rename / move group / delete run in the background and come back when done along with the reread sessions (rereading takes
    // tens of milliseconds too, kept off the interface)
    let (job_tx, job_rx) = mpsc::channel::<(Job, Outcome, Vec<Session>)>();
    // Background refreshes held back while sliding
    let mut pending: Option<Vec<Session>> = None;
    // Diff view: git diff finished in the background
    let (diff_tx, diff_rx) = mpsc::channel::<(String, Mode, diff::Diff)>();
    let (plug_tx, plug_rx) = mpsc::channel::<(crate::caps::Req, bool, String)>();
    let mut diff_busy = false;
    let (glance_tx, glance_rx) = mpsc::channel::<(String, Glance)>();
    let mut glance_busy = false;
    // Session record: the first full read (the largest over a hundred MB, a fraction of a second) is in the background; later additions are
    // read in place on refresh
    let (trace_tx, trace_rx) = mpsc::channel::<(String, Result<crate::trace::Trace, String>)>();
    // Billing panel: the child process aca ui --cost-json does the reading (it needs a hundred-odd MB of memory while reading, returned
    // when the child exits); this thread waits for its result
    let (bill_tx, bill_rx) = mpsc::channel::<Result<aca_records::usage::Report, String>>();
    let (bill_req_tx, bill_req_rx) = mpsc::channel::<Vec<String>>();
    thread::spawn(move || {
        while let Ok(only) = bill_req_rx.recv() {
            if bill_tx.send(read_bill(&only)).is_err() {
                break;
            }
        }
    });

    let mut quiet_until = Instant::now() + startup_quiet(crate::data::in_popup() || restarted.is_some());
    // ratatui::init installs its own panic cleanup; take out the original one (std's default) first, and after init swap in exitlog's
    let std_hook = std::panic::take_hook();
    let mut terminal = ratatui::init();
    crate::exitlog::install(std_hook, release_terminal);
    execute!(stdout(), EnableMouseCapture)?;
    focus_events(true);
    // In a tmux pane / opened directly in kitty: switching to another pane or app gives the input method back (popups get no focus events)
    let mut focused = true;
    // Ask only in terminals known to support it: asking waits for the terminal's answer, and with a phone behind mosh that never answers,
    // every open would wait 2 seconds for nothing
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let likely = !env("KITTY_WINDOW_ID").is_empty()
        || ["ghostty", "WezTerm"].contains(&env("TERM_PROGRAM").as_str())
        || ["kitty", "ghostty", "foot"].iter().any(|t| env("TERM").contains(t));
    let ok = likely && matches!(ratatui::crossterm::terminal::supports_keyboard_enhancement(), Ok(true));
    KBD.store(ok, Ordering::Relaxed);
    kbd(true);
    // Tell kitty the highlighted item's directory and session (see `tell_var`); inside tmux it wouldn't reach kitty. told = what was told
    // last, rewrite only when it changes
    let tell = !env("KITTY_WINDOW_ID").is_empty() && env("TMUX").is_empty();
    let mut told = (String::new(), String::new());
    let res = (|| -> anyhow::Result<()> {
        loop {
            // Terminal gone (window closed, ssh dropped): don't wait for a read / write error, clean up and exit (see exitlog.rs)
            if crate::exitlog::gone() {
                return Ok(());
            }
            // A background thread failed: the interface is still up, say so in the bottom bar
            if let Some(m) = crate::exitlog::take_live() {
                app.flash_for(&m, NOTICE);
            }
            let mut refreshed = false;
            // Just moved: hold it and swap in once you stop: a full redraw in the middle of a slide is a stutter on a phone
            while let Ok(v) = rx.try_recv() {
                pending = Some(v);
            }
            if let Some(v) = pending.take_if(|_| !app.just_moved()) {
                app.set_sessions(v);
                refreshed = true;
            }
            while let Ok(a) = alert_rx.try_recv() {
                app.plugin_alerts = a;
            }
            while let Ok((job, out, fresh)) = job_rx.try_recv() {
                app.job_done(&job, out, fresh);
                refreshed = true;
            }
            while let Ok(t) = idle_rx.try_recv() {
                if !t.stopped.is_empty() {
                    app.flash_for(&idle_note(&t.stopped), NOTICE);
                    let _ = req_tx.send(());
                }
                if let Some((after, archived)) = t.remove {
                    app.sweep(after, aca_core::paths::now(), archived);
                }
            }
            // Full-screen reader: just opened / session changed / every timed refresh, refetch the history
            // (even 3000 lines take only a few ms)
            if let Some(target) = app.reader_target().filter(|_| std::mem::take(&mut app.reader_load) || refreshed) {
                app.set_reader_lines(crate::data::capture_history(&target));
            }
            // Plugin capabilities / views / panes: one thread each, sent back when done
            for req in std::mem::take(&mut app.plugin_q) {
                let tx = plug_tx.clone();
                thread::spawn(move || {
                    let (ok, out) = crate::caps::run(&req);
                    let _ = tx.send((req, ok, out));
                });
            }
            while let Ok((req, ok, out)) = plug_rx.try_recv() {
                app.plugin_done(req, ok, out);
            }
            // Diff view: git runs in the background (a big repo takes a while), only one at a time
            while let Ok((name, mode, d)) = diff_rx.try_recv() {
                diff_busy = false;
                app.set_diff(&name, mode, d);
            }
            if !diff_busy && let Some((name, w, mode)) = app.diff_request(refreshed) {
                diff_busy = true;
                let tx = diff_tx.clone();
                thread::spawn(move || {
                    let _ = tx.send((name, mode, diff::load(&w, mode)));
                });
            }
            while let Ok((name, r)) = trace_rx.try_recv() {
                app.set_trace(&name, r);
            }
            if let Some(s) = app.trace_request() {
                let tx = trace_tx.clone();
                thread::spawn(move || {
                    let r = crate::trace::load(&s);
                    let _ = tx.send((s.name.clone(), r));
                });
            }
            if refreshed {
                app.trace_tail();
            }
            // Diff stats at the top of the preview: for the highlighted one, also read in the background
            while let Ok((name, g)) = glance_rx.try_recv() {
                glance_busy = false;
                app.set_glance(name, g);
            }
            if !glance_busy && let Some((name, w)) = app.glance_request(refreshed) {
                glance_busy = true;
                let tx = glance_tx.clone();
                // A group's preview also lists what's in the directory
                let group = is_group_key(&name);
                thread::spawn(move || {
                    let g = if group { diff::glance_dir(&w) } else { diff::glance(&w) };
                    let _ = tx.send((name, g));
                });
            }
            while let Ok(r) = bill_rx.try_recv() {
                app.set_bill(r);
            }
            if let Some(only) = app.bill_request() {
                let _ = bill_req_tx.send(only);
            }
            crate::ime::english(focused && !app.typing());
            // Colors changed in config.toml get swapped in; so does the input method switch
            // (changes in the settings page take effect at once)
            let cfg_changed = theme::refresh();
            if cfg_changed || std::mem::take(&mut app.ime_changed) {
                let was = crate::ime::active();
                crate::ime::reconfigure();
                match (was, crate::ime::active()) {
                    (false, true) => execute!(stdout(), EnableFocusChange)?,
                    (true, false) => execute!(stdout(), DisableFocusChange)?,
                    _ => {}
                }
            }
            if cfg_changed {
                i18n::reload();
                app.reload_settings();
            }
            if i18n::lang() != lang {
                lang = i18n::lang();
                app.lang_changed();
            }
            terminal.draw(|f| draw(f, &mut app))?;
            if tell {
                let here = (app.here_dir(), app.here_sid());
                if here.0 != told.0 {
                    tell_var("aca_dir", &here.0);
                }
                if here.1 != told.1 {
                    tell_var("aca_sid", &here.1);
                }
                told = here;
            }
            // While sliding, wake up at the time the preview should switch, so it switches on time once you stop
            let mut wait =
                app.settle_left().map_or(Duration::from_millis(150), |d| d.min(Duration::from_millis(150)) + Duration::from_millis(5));
            // Billing is loading: it's back in tens of milliseconds, don't wait the full 150 ms to swap it in
            if app.bill_loading {
                wait = wait.min(Duration::from_millis(15));
            }
            if wait_input(wait, app.term_on.is_some())? {
                // Read all the events that have arrived at once. 3 or more keys arriving together = not typed by hand: after connecting,
                // Moshi types commands into the shell for you (like cd to the latest directory), but aca is what's taking input then — the
                // "/" in it enters filter mode, the path becomes the filter, Enter confirms it, and the list is filtered to empty, looking
                // like a stuck fzf. Pastes likewise. Drop the whole batch.
                let mut batch = vec![event::read()?];
                while batch.len() < 4096 && event::poll(Duration::ZERO)? {
                    batch.push(event::read()?);
                }
                // The sequence kitty sends back when a desktop notification is clicked isn't keys: pick it out first (else the whole
                // sequence would be dropped as typed-for-you, and muted for a while)
                for osc in crate::clicked::take(&mut batch) {
                    if let Some(id) = crate::clicked::session(&osc) {
                        app.open_clicked(id);
                    }
                }
                let keys: Vec<KeyEvent> = batch
                    .iter()
                    .filter_map(|e| match e {
                        Event::Key(k) if k.kind == KeyEventKind::Press => Some(*k),
                        _ => None,
                    })
                    .collect();
                let now = Instant::now();
                let drop = drops(&keys, now < quiet_until, app.typing());
                if drop {
                    // Log only, no hint in the bottom bar
                    log_ignored(&keys);
                    // Only one key dropped = pressed by a person during the quiet period, don't extend:
                    // extending would swallow every key of a fast typist
                    if keys.len() > 1 {
                        quiet_until = quiet_until.max(now + BURST_QUIET);
                    }
                }
                for e in batch {
                    match e {
                        Event::Key(k) if k.kind == KeyEventKind::Press && !drop => app.on_key(k),
                        Event::Mouse(m) => app.on_mouse(m),
                        Event::FocusLost => focused = false,
                        Event::FocusGained => focused = true,
                        _ => {}
                    }
                }
            }
            if std::mem::take(&mut app.refresh) {
                let _ = req_tx.send(());
            }
            if let Some(job) = app.take_job() {
                let tx = job_tx.clone();
                thread::spawn(move || {
                    let out = ops::run(&job);
                    let _ = tx.send((job, out, collect()));
                });
            }
            // Picked: into the pane. Opened by `aca ui --ref`: close (the popup goes with it); started in the list: back to the row it
            // went into, its preview shows it
            if let (Some((text, of)), Some(pane)) = (app.put.take(), app.insert_to.clone()) {
                match crate::data::paste_into(&pane, &text) {
                    Ok(()) if app.insert_back.is_none() => return Ok(()),
                    Ok(()) => {
                        app.end_insert();
                        app.flash(&tr!("已插入「{of}」的引用", "inserted the reference to “{of}”"));
                    }
                    Err(e) => app.flash(&tr!("无法插入:{e}", "couldn't insert: {e}")),
                }
            }
            if let Some(c) = app.clip.take() {
                let (_, what, text) = match c {
                    Clip::Last { id, title, screen, .. } => {
                        let got = (!id.is_empty()).then(|| create::last_response(&id)).flatten();
                        let what =
                            if got.is_some() { t!("最后一条回复", "the last reply") } else { t!("屏幕上的内容", "the screen") };
                        (title, what, got.unwrap_or(screen))
                    }
                    Clip::Info { title, text } => (title, t!("会话信息", "session info"), text),
                    Clip::Ref { title, text } => (title, t!("引用", "the reference"), text),
                    Clip::Text(text) => (String::new(), "", text),
                };
                let m = if text.trim().is_empty() {
                    t!("没有可复制的内容", "nothing to copy").to_string()
                } else {
                    match crate::clip::copy(&text) {
                        Ok(_) if what.is_empty() => tr!("已复制 {text}", "copied {text}"),
                        Ok(_) => tr!("已复制{what}({} 行)", "copied {what} ({} lines)", text.lines().count()),
                        Err(e) => tr!("复制失败:{e}", "copy failed: {e}"),
                    }
                };
                app.flash(&m);
            }
            // Save the view as soon as it changes (when the connection drops aca is killed outright, never reaching exit)
            let now_view = app.view_state();
            if now_view != saved && !app.just_moved() {
                if let Some(p) = &view_file {
                    let _ = now_view.save(p);
                }
                saved = now_view;
            }
            // Following: once the highlight settles, take this client there; Esc takes it back to where it was, then exits
            if let Some(to) = app.follow_step() {
                crate::data::show(&to.session, &to.pane);
            }
            if app.quit {
                app.follow_done();
                return Ok(());
            }
            // fzf before new: Enter in fzf opens it, and new follows right away
            if std::mem::take(&mut app.pick) {
                let size = terminal.size()?;
                if let Some(p) = app.dir_pick(size.width, size.height) {
                    paused.store(true, Ordering::Relaxed);
                    suspend()?;
                    let got = create::pick_dir(&p);
                    app.picked(got);
                    resume(&mut terminal, &mut app)?;
                    paused.store(false, Ordering::Relaxed);
                }
            }
            if let Some(req) = app.create.take() {
                app.flash(t!("正在新建…", "creating…"));
                terminal.draw(|f| draw(f, &mut app))?;
                match create::create(&req) {
                    Ok(id) => {
                        app.adopt(&id);
                        app.attach = Some(id); // go on to the attach below
                    }
                    Err(e) => app.flash(&tr!("新建失败:{e}", "creating failed: {e}")),
                }
                // Keys held up while waiting weren't meant for the new session
                while event::poll(Duration::ZERO)? {
                    let _ = event::read()?;
                }
                let _ = req_tx.send(());
            }
            if let Some(t) = app.start.take() {
                app.flash(&tr!("正在启动「{}」…", "starting “{}”…", clip(&t.title, 16).trim_end()));
                terminal.draw(|f| draw(f, &mut app))?;
                match create::start(&t.id) {
                    Ok(()) => app.attach = Some(t.name),
                    Err(e) => app.flash(&tr!("启动失败:{e}", "start failed: {e}")),
                }
                while event::poll(Duration::ZERO)? {
                    let _ = event::read()?;
                }
                let _ = req_tx.send(());
            }
            if let Some((family, p)) = app.respawn.take() {
                match crate::park::respawn(&p) {
                    Ok(()) => {
                        app.focus = Some(p.id.clone());
                        app.attach = Some(family);
                    }
                    Err(e) => app.flash(&tr!("启动失败:{e}", "start failed: {e}")),
                }
                let _ = req_tx.send(());
            }
            if std::mem::take(&mut app.purge) {
                thread::spawn(crate::trash::purge);
            }
            if let Some(ed) = app.edit_note.take() {
                paused.store(true, Ordering::Relaxed);
                suspend()?;
                let got = notes::edit_in_editor(&ed.text);
                app.note_editor_done(ed, got);
                resume(&mut terminal, &mut app)?;
                paused.store(false, Ordering::Relaxed);
            }
            // yazi in a tmux popup: it detects tmux and passes through to ask the outer terminal, the popup doesn't forward that, and every
            // exit waits 5 seconds for nothing (no images either) → instead run it in a zoomed split in the session; q quits and closes
            // that pane, leaving you in the session (the popup is for switching over anyway)
            if let Some(External::Yazi(sp)) =
                app.external.take_if(|x| matches!(x, External::Yazi(sp) if !sp.target.is_empty()) && crate::data::in_popup())
            {
                app.split = Some(sp);
            }
            if let Some(x) = app.external.take() {
                paused.store(true, Ordering::Relaxed);
                suspend()?;
                let err = run_external(&x);
                app.external_done(err);
                resume(&mut terminal, &mut app)?;
                paused.store(false, Ordering::Relaxed);
                let _ = req_tx.send(());
            }
            if let Some(p) = app.pick_group.take() {
                paused.store(true, Ordering::Relaxed);
                suspend()?;
                let got = ops::pick_group(&p.rows, &p.what(), &p.here);
                app.group_picked(p, got);
                resume(&mut terminal, &mut app)?;
                paused.store(false, Ordering::Relaxed);
            }
            if let Some(r) = app.term_req.take() {
                let dir = aca_records::workdir::find(&r.w).dir;
                if std::path::Path::new(&dir).is_dir() {
                    // A group's terminal belongs to no session: no account
                    let env = if !r.env.is_empty() || r.family.is_empty() {
                        r.env.clone()
                    } else {
                        let head = app.all.iter().find(|s| s.sub.is_none() && s.name == r.family);
                        crate::data::account_env(head, r.w.pid, r.w.agent)
                    };
                    // Open at half screen first, then resize to the terminal area's exact size when drawing the first frame
                    let (rows, cols) = (app.screen.height / 2, app.screen.width);
                    let spawned = match &r.run {
                        Some(c) => crate::term::Term::spawn_cmd(c, &dir, &env, rows, cols),
                        None => crate::term::Term::spawn(&dir, &env, rows, cols),
                    };
                    match spawned {
                        Ok(mut t) => {
                            t.title = r.title;
                            t.link = r.link;
                            app.terms.insert(r.key.clone(), t);
                            app.term_on = Some(r.key);
                        }
                        Err(e) => app.flash(&tr!("无法打开终端:{e}", "can't open the terminal: {e}")),
                    }
                } else {
                    app.flash(&if dir.is_empty() {
                        t!("无法确定会话目录", "no idea which directory it's in").to_string()
                    } else {
                        tr!("目录已不存在:{dir}", "the directory is gone: {dir}")
                    });
                }
            }
            // Remove ones whose shell exited; if the one being viewed is gone, back to the list
            app.terms.retain(|_, t| !t.exited());
            app.catch_links();
            if app.term_on.as_ref().is_some_and(|k| !app.terms.contains_key(k)) {
                app.term_on = None;
                // The terminal area gone means it was closed: nothing more to say
            }
            if let Some(sp) = app.split.take() {
                let dir = aca_records::workdir::find(&sp.w).dir;
                let session = app.tmux_of(&sp.name);
                // A session row doesn't know the pane id: split its current pane
                let target = if sp.target.is_empty() { format!("{session}:") } else { sp.target.clone() };
                match sp.yazi.then(|| yazi_blocked(&dir)).flatten() {
                    Some(e) => app.flash(&e),
                    None => match if sp.yazi {
                        crate::data::split(&session, &target, &dir, sp.side, true, &sp.env)
                    } else {
                        crate::data::new_window(&session, &dir, &sp.env)
                    } {
                        Some(pane) => {
                            app.focus = Some(pane);
                            app.attach = Some(sp.name);
                        }
                        None => app.flash(if sp.yazi {
                            t!("无法分屏:会话可能刚结束", "can't split: the session may have just ended")
                        } else {
                            t!("无法打开窗口:会话可能刚结束", "can't open a window: the session may have just ended")
                        }),
                    },
                }
            }
            if let Some(name) = app.attach.take() {
                // name is the session's key; tmux looks it up by session number (the session name is the title, which changes)
                let target = app.tmux_of(&name);
                // Switch to that pane first: the session itself lands on the main pane (where the agent is),
                // a child row on that window / pane
                let focus = app.focus.take();
                if let Some(p) = &focus {
                    crate::data::focus_pane(p);
                }
                // E: paste the note into its input box first (no Enter), so you see it on entering
                if let Some((key, text)) = app.fill.take()
                    && crate::data::paste_into(&focus.unwrap_or_else(|| format!("{target}:")), &text).is_ok()
                {
                    app.note_filled(&key);
                }
                if std::env::var_os("TMUX").is_some() {
                    // Already in tmux (in tmux on desktop, a popup on a phone): switch over directly, no nesting. Switching away exits the
                    // interface; going in counts as seen. Switch first, then hand over the terminal: leave full screen only once the
                    // interface is out of sight, so the shell below isn't seen
                    app.mark_seen(&name);
                    aca_core::tmux::bind_detach(aca_core::plugin::hotkey_binds);
                    crate::data::switch_to(&target)?;
                    suspend()?;
                    return Ok(());
                }
                // Prepare before handing over the terminal (the interface is still on screen), so tmux comes up showing it
                let entry = crate::data::prepare(&target);
                paused.store(true, Ordering::Relaxed);
                suspend()?;
                let left = crate::data::go_in(&entry);
                app.came_back(&name, left.as_deref().unwrap_or_default());
                if left.is_none() {
                    app.flash(t!("无法进入:会话可能刚结束", "can't go in: the session may have just ended"));
                }
                // Draw the first frame back with the list from before going in;
                // the new one is read in the background (tens of ms) and swapped in
                resume(&mut terminal, &mut app)?;
                quiet_until = Instant::now() + BACK_QUIET;
                paused.store(false, Ordering::Relaxed);
                let _ = req_tx.send(());
                if let Some(left) = left {
                    crate::data::remember_left(&entry, &left);
                }
            }
        }
    })();
    kbd(false);
    focus_events(false);
    if !told.0.is_empty() {
        tell_var("aca_dir", "");
    }
    if !told.1.is_empty() {
        tell_var("aca_sid", "");
    }
    crate::ime::english(false);
    let _ = execute!(stdout(), DisableMouseCapture);
    // Terminal gone (window closed, ssh dropped): it can't be given back and there's nowhere to say so: log to exit.log, don't print, don't
    // pass the error on. This used to use ratatui::restore(), which printed to the dead terminal when it couldn't restore, error on error,
    // and the program aborted (see exitlog.rs)
    let restored = ratatui::try_restore();
    let gone = restored.is_err() || crate::exitlog::gone();
    if gone {
        crate::exitlog::term_gone(res.as_ref().err(), restored.as_ref().err());
    }
    // Trash entries that are due: really delete (in a separate process, so exit isn't delayed; deleted even if the terminal is gone)
    if app.trash.due(epoch()) {
        crate::trash::purge_detached();
    }
    if gone {
        return Ok(());
    }
    if app.relaunch && res.is_ok() {
        // A highlight that just moved may not be saved yet (saved once you stop): save it once more before switching
        if let Some(p) = &view_file {
            let _ = app.view_state().save(p);
        }
        return Err(relaunch(&app.relaunch_state()));
    }
    res
}

/// For debugging: render one frame of the given size in memory, feed the keys in keys in order, print it as text (CJK counted as two
/// cells). Draws a frame per key like the interface: some state (which steps of the record can unfold) is only computed while drawing
pub fn snapshot(w: u16, h: u16, keys: &str) -> anyhow::Result<String> {
    // Shows the real pinned ones, but p only changes memory, doesn't write the file
    let mut app = App::new(String::new(), collect())
        .with_pins(Pins::open(Pins::default_path()).detached())
        .with_notes(Notes::open(Notes::default_path()).detached())
        .with_marks(Reads::open(Reads::default_path()).detached(), Spaces::open(Spaces::default_path()).detached())
        .with_trash(Trash::open(Trash::default_path()).detached())
        .with_group_hints(crate::data::group_hints)
        .with_adopt(adopt_plan)
        .with_new_group_dir(crate::config::new_group_dir)
        .with_tools(|| Ok(crate::tools::load()))
        .with_mcp(crate::mcp::load)
        .with_groups(crate::groups::load)
        .with_handoff(crate::handoff::source)
        .with_rehome(crate::rehome::quotas, crate::accounts::login, true);
    let mut t = Terminal::new(TestBackend::new(w, h))?;
    t.draw(|f| draw(f, &mut app))?;
    for c in keys.chars() {
        let code = match c {
            '↓' => KeyCode::Down,
            '↑' => KeyCode::Up,
            '→' => KeyCode::Right,
            '←' => KeyCode::Left,
            '⏎' => KeyCode::Enter,
            '⎋' => KeyCode::Esc,
            '⇥' => KeyCode::Tab,
            '⇤' => KeyCode::BackTab,
            '⌫' => KeyCode::Backspace,
            c => KeyCode::Char(c),
        };
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        // Read in the background in the interface; here it's read on the spot, so the following keys (n / o / →) land on the record
        if let Some(s) = app.trace_request() {
            app.set_trace(&s.name, crate::trace::load(&s));
        }
        t.draw(|f| draw(f, &mut app))?;
    }
    // The frame handed out is drawn on a clean screen: the in-memory screen updates only diffs, so drawing several frames in a row leaves
    // half a wide char from the previous frame
    let mut t = Terminal::new(TestBackend::new(w, h))?;
    t.draw(|f| draw(f, &mut app))?;
    let buf = t.backend().buffer();
    let mut out = String::new();
    for y in 0..h {
        let mut x = 0;
        while x < w {
            let cell = &buf[(x, y)];
            out.push_str(&ansi::strip_osc8(cell.symbol()));
            x += cell.cell_width().max(1);
        }
        if (0..w).any(|x| buf[(x, y)].modifier.contains(Modifier::REVERSED)) {
            out.push_str(" ◀高亮");
        }
        out.push('\n');
    }
    if let Some(a) = app.attach {
        out.push_str(&format!("[attach → {a}]\n"));
    }
    if let Some(sp) = &app.split {
        let how = if !sp.yazi {
            "新窗口"
        } else if sp.side {
            "左右分"
        } else {
            "上下分"
        };
        out.push_str(&format!("[{how} → {}]\n", sp.name));
    }
    if let Some(j) = &app.job {
        out.push_str(&format!("[后台 → {}]\n", j.doing()));
    }
    if let Some(p) = &app.pick_group {
        let groups: Vec<&str> = p.rows.iter().map(|r| r.group.as_str()).collect();
        out.push_str(&format!("[挑分组 → {}]\n", groups.join(" ")));
    }
    // Print only, don't really create
    if let Some(r) = app.create {
        let mut flags = String::new();
        if !r.group.is_empty() {
            flags += &format!(" -g {}", r.group);
        }
        if r.create_dir {
            flags += " (先建目录)";
        }
        out.push_str(&format!("[新建 → aca new {} {}{flags}]\n", r.spec(), words::tilde(&r.dir)));
    }
    Ok(out)
}
