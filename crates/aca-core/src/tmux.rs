//! The tmux layer: tmux holds the sessions, and aca only keeps records on top.
//!
//! Sessions aren't recognized by name: on creation the tmux session gets `@aca_id`, and the agent's pane gets `@aca_agent`.
//! So **the session name can simply be the title** and change freely (it's what clients like Moshi show in their list),
//! and commands always use tmux's own `$<session>` / `%<pane>` ids, so no character in a name can cause trouble.
//!
//! `ACA_TMUX_SOCKET` switches to another tmux server (`tmux -L`): tests only connect to the one with fake sessions and
//! can't touch the real ones.

use std::process::{Command, Stdio};

use crate::paths::Res;
use crate::{t, tr};

pub fn cmd() -> Command {
    let mut c = Command::new("tmux");
    if let Some(s) = std::env::var("ACA_TMUX_SOCKET").ok().filter(|s| !s.is_empty()) {
        c.args(["-L", &s]);
    }
    c.stdin(Stdio::null());
    c
}

/// Runs one command: stdout on success, tmux's message on failure
pub fn run<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Res<String> {
    let o = cmd().args(args).output().map_err(|e| tr!("无法启动 tmux:{e}", "can't run tmux: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// tmux treats a lone `;` and a trailing `;` as command separators; escape them in arguments passed to it
pub fn arg(a: &str) -> String {
    match a.strip_suffix(';') {
        Some(head) => format!("{head}\\;"),
        None => a.to_string(),
    }
}

/// A pane, plus a few things about its session
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pane {
    /// `@aca_id`: empty = not created by aca
    pub id: String,
    /// `@aca_agent`: the pane the agent is in
    pub agent: bool,
    /// tmux's session id (`$3`), for commands
    pub sid: String,
    pub session: String,
    pub pane: String,
    pub pid: u32,
    pub cmd: String,
    pub dead: bool,
    pub attached: u32,
    /// When it was last attached and last touched (just passing through doesn't count, see [`attach_via`])
    pub last_attached: u64,
    pub activity: u64,
    pub window: u32,
    pub index: u32,
    pub path: String,
    /// When this window last had output (the session's `activity` is when someone last touched it, not output)
    pub win_activity: u64,
    /// How many panes this window has (>1 = split) and the window name
    pub win_panes: u32,
    pub win_name: String,
    /// The pane title the program writes itself (Claude puts a spinner at the front while working)
    pub title: String,
}

const FMT: &str = "#{@aca_id}\t#{@aca_agent}\t#{session_id}\t#{session_name}\t#{pane_id}\t#{pane_pid}\t#{pane_current_command}\t#{pane_dead}\t#{session_attached}\t#{session_last_attached}\t#{session_activity}\t#{window_index}\t#{pane_index}\t#{pane_current_path}\t#{window_activity}\t#{window_panes}\t#{window_name}\t#{@aca_pass}\t#{pane_title}";

pub fn parse_panes(out: &str) -> Vec<Pane> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(19, '\t').collect();
            (f.len() == 19).then(|| {
                let (last_attached, activity) = unpass(f[9].parse().unwrap_or(0), f[10].parse().unwrap_or(0), f[17]);
                Pane {
                    id: f[0].into(),
                    agent: f[1] == "1",
                    sid: f[2].into(),
                    session: f[3].into(),
                    pane: f[4].into(),
                    pid: f[5].parse().unwrap_or(0),
                    cmd: f[6].into(),
                    dead: f[7] == "1",
                    attached: f[8].parse().unwrap_or(0),
                    last_attached,
                    activity,
                    window: f[11].parse().unwrap_or(0),
                    index: f[12].parse().unwrap_or(0),
                    path: f[13].into(),
                    win_activity: f[14].parse().unwrap_or(0),
                    win_panes: f[15].parse().unwrap_or(1),
                    win_name: f[16].into(),
                    title: f[18].into(),
                }
            })
        })
        .collect()
}

/// Attach to `to` from outside, passing through `via` first (the one this terminal just left): that's how tmux remembers
/// `via` as this client's previous session, so `switch-client -l` (your ^` binding) gets back to it after going in. But
/// tmux records passing through as attached and touched too, so "unread" (◐) would be lost and the auto-stop clock would
/// restart. So the same command records on `via`'s `@aca_pass` the two times from before passing through plus the moment
/// of passing (`attached/touched/passed`); when panes are read, if the last attach is this pass, the times from before it
/// are used (see [`unpass`])
pub fn attach_via(via: &str, to: &str) -> Vec<String> {
    let before = ["set-option", "-F", "-t", via, "@aca_pass", "#{session_last_attached}/#{session_activity}"];
    let after = ["set-option", "-aF", "-t", via, "@aca_pass", "/#{session_last_attached}"];
    [&before[..], &[";", "attach-session", "-t", via, ";"], &after, &[";", "switch-client", "-t", to]]
        .concat()
        .into_iter()
        .map(String::from)
        .collect()
}

/// The session has a pass recorded (`@aca_pass`) and nobody attached since: swap the attached and touched times back to
/// those from before the pass. If only the first half was recorded (attaching to `via` failed), or someone attached since,
/// go by what tmux says
fn unpass(last_attached: u64, activity: u64, pass: &str) -> (u64, u64) {
    let f: Vec<u64> = pass.split('/').map(|x| x.parse().unwrap_or(0)).collect();
    match f[..] {
        [attached, active, at] if at > 0 && at == last_attached && activity <= at => (attached, active),
        _ => (last_attached, activity),
    }
}

/// All panes; tmux server not running = empty
pub fn panes() -> Vec<Pane> {
    run(&["list-panes", "-a", "-F", FMT]).map(|o| parse_panes(&o)).unwrap_or_default()
}

/// A session's agent pane: the one marked `@aca_agent`; if there is none (old session, someone closed it), the one with
/// the lowest index
pub fn agent_pane<'a>(panes: &'a [Pane], id: &str) -> Option<&'a Pane> {
    let mine = || panes.iter().filter(|p| p.id == id);
    mine().find(|p| p.agent).or_else(|| mine().min_by_key(|p| (p.window, p.index)))
}

pub fn find(id: &str) -> Option<Pane> {
    agent_pane(&panes(), id).cloned()
}

/// Which aca session this pane is in (the `@aca_id` visible from it); not an aca session, or can't ask = None
pub fn session_of(pane: &str) -> Option<String> {
    run(&["display-message", "-p", "-t", pane, "#{@aca_id}"]).ok().map(|o| o.trim().to_string()).filter(|s| !s.is_empty())
}

/// Title → tmux session name: `.` and `:` mean something in tmux targets (`session:window.pane`), so replace them;
/// collapse whitespace into one; truncate if too long
pub fn safe_name(title: &str) -> String {
    let t: String = title
        .chars()
        .map(|c| match c {
            '.' | ':' => '_',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    let t: String = t.chars().take(48).collect();
    if t.is_empty() { "aca".into() } else { t }
}

/// Duplicates get ` (2)`, ` (3)` …
pub fn unique_name(want: &str, taken: &[String]) -> String {
    let mut name = want.to_string();
    let mut n = 2;
    while taken.contains(&name) {
        name = format!("{want} ({n})");
        n += 1;
    }
    name
}

/// Makes the session name follow the title (the agent window's name too): Moshi lists session names at the first level
/// and window names at the second
pub fn sync_name(id: &str, title: &str) -> Option<String> {
    let all = panes();
    let p = agent_pane(&all, id)?;
    let mut taken: Vec<String> = all.iter().filter(|x| x.sid != p.sid).map(|x| x.session.clone()).collect();
    taken.dedup();
    let name = unique_name(&safe_name(title), &taken);
    if name != p.session {
        run(&["rename-session", "-t", &p.sid, &name]).ok()?;
    }
    let _ = run(&["rename-window", "-t", &p.pane, &name]);
    Some(name)
}

/// The text on screen now (no colors): for telling the state
pub fn capture(pane: &str) -> Vec<String> {
    plain_lines(&run(&["capture-pane", "-p", "-t", pane]).unwrap_or_default())
}

/// `capture-pane -p` output → lines of text: no trailing whitespace on lines, no trailing empty lines
pub fn plain_lines(out: &str) -> Vec<String> {
    let mut lines: Vec<String> = out.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// Captures several panes' screens with one tmux command (`ansi` = with colors, `capture-pane -e`): same order as
/// `panes`, one chunk per pane, exactly the text capturing it alone would print; a pane that can't be captured is empty.
/// One tmux per pane would be thirty-odd processes per list refresh (measured 2026-10-14: 37 panes, 143 ms CPU; merged
/// into one, 3.6 ms)
pub fn capture_many(panes: &[&str], ansi: bool) -> Vec<String> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    // The mark is made up fresh every time: even if a line on the screen happens to match (this source file open), the split
    // stays right
    let mark = format!("aca-cut-{:x}-{:x}-{:x}", std::process::id(), nanos, N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    capture_with(panes, ansi, &mark, |args| {
        cmd().args(args).stderr(Stdio::null()).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    })
}

/// After each pane's capture, a `display-message -p` prints a mark line, used to split the output back up. The mark can't
/// contain `#` or `%` (display-message expands them as formats) or a trailing `;`
fn capture_args(panes: &[&str], ansi: bool, mark: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![];
    for p in panes {
        if !args.is_empty() {
            args.push(";".into());
        }
        args.extend(["capture-pane", "-p"].map(String::from));
        if ansi {
            args.push("-e".into());
        }
        args.extend(["-t", p, ";", "display-message", "-p", "-t", p, mark].map(String::from));
    }
    args
}

/// Splits the long output at the mark lines; a last half-chunk that never got its mark doesn't count
fn split_marked(out: &str, mark: &str) -> Vec<String> {
    let (mut segs, mut cur) = (vec![], String::new());
    for l in out.split_inclusive('\n') {
        if l.trim_end_matches(['\n', '\r']) == mark {
            segs.push(std::mem::take(&mut cur));
        } else {
            cur.push_str(l);
        }
    }
    segs
}

/// If one pane can't be captured (just closed), tmux stops there and doesn't run the rest: record that pane as empty and
/// carry on from the next. Each round moves at least one pane forward, so even if none can be captured it's one process
/// per pane (same as before)
fn capture_with(panes: &[&str], ansi: bool, mark: &str, mut run: impl FnMut(&[String]) -> String) -> Vec<String> {
    let mut got: Vec<String> = Vec::with_capacity(panes.len());
    while got.len() < panes.len() {
        let rest = &panes[got.len()..];
        let segs = split_marked(&run(&capture_args(rest, ansi, mark)), mark);
        let whole = segs.len() >= rest.len();
        got.extend(segs.into_iter().take(rest.len()));
        if !whole {
            got.push(String::new());
        }
    }
    got
}

/// Text for plugins (`input = "screen" / "scrollback"`): wrapped lines joined back into one (URLs don't break), `back` =
/// including scrollback (at most this many lines)
pub fn text(pane: &str, back: bool) -> String {
    let mut args = vec!["capture-pane", "-p", "-J", "-t", pane];
    if back {
        args.extend(["-S", "-10000"]);
    }
    let out = run(&args).unwrap_or_default();
    let lines: Vec<&str> = out.lines().map(str::trim_end).collect();
    let end = lines.iter().rposition(|l| !l.is_empty()).map_or(0, |i| i + 1);
    lines[..end].iter().map(|l| format!("{l}\n")).collect()
}

/// The screen with colors (`capture-pane -e`): telling which text in the input box is gray (a hint) and which a person
/// typed takes the colors
pub fn capture_ansi(pane: &str) -> Vec<String> {
    run(&["capture-pane", "-p", "-e", "-t", pane]).unwrap_or_default().lines().map(String::from).collect()
}

/// Whether you're typing in this pane right now: a terminal is looking at it, and that terminal's last key press was within
/// `secs` seconds. tmux's "last key press" only changes with real key presses in the terminal; aca's own send-keys and
/// pastes don't change it (tested 2026-10-08)
pub fn typing(pane: &str, secs: u64) -> bool {
    let now = crate::paths::now();
    run(&["list-clients", "-F", "#{client_activity}\t#{pane_id}"]).unwrap_or_default().lines().any(|l| {
        let (at, p) = l.split_once('\t').unwrap_or_default();
        p == pane && now.saturating_sub(at.parse().unwrap_or(0)) < secs
    })
}

/// Presses a few keys in a pane (tmux key names: `2`, `Escape`, `Enter`)
pub fn keys(pane: &str, keys: &[&str]) -> Res<()> {
    let mut a = vec!["send-keys", "-t", pane];
    a.extend_from_slice(keys);
    run(&a).map(|_| ())
}

/// Pastes text into the pane's input box (bracketed paste, so multiple lines aren't taken as sending), without pressing
/// Enter. If the pane is in copy-mode, leave it first: text pasted then would be typed key by key to tmux
pub fn paste(pane: &str, text: &str) -> Res<()> {
    let in_mode = || run(&["display-message", "-p", "-t", pane, "#{pane_in_mode}"]).is_ok_and(|o| o.trim() == "1");
    if in_mode() {
        let _ = run(&["copy-mode", "-q", "-t", pane]);
        if in_mode() {
            return Err(t!("窗格处于 tmux 模式", "the pane is in a tmux mode").into());
        }
    }
    let buf = format!("aca-{}", std::process::id());
    run(&["set-buffer", "-b", &buf, "--", text])?;
    run(&["paste-buffer", "-p", "-d", "-b", &buf, "-t", pane]).map(|_| ())
}

const DETACH_KEY: &str = "C-q";
/// Sessions where pressing it gets you back: those marked `@aca_id`, and those named by agent-deck
const DETACH_IN: &str = "#{||:#{m:agentdeck_*,#{session_name}},#{?@aca_id,1,0}}";
/// `after-bind-key` is an array, and aca takes only this slot: what you or agent-deck hook in sit in other slots and
/// aren't touched
const DETACH_HOOK: &str = "after-bind-key[222]";
/// In the hook: the key just bound is this one, and it's agent-deck's version (recognizes its own session names, not
/// `@aca_id`). Your own bindings to something else don't count
const DECK_BOUND: &str =
    "#{&&:#{==:#{hook_argument_0},C-q},#{&&:#{m:*agentdeck_*,#{hook_arguments}},#{==:#{m:*@aca_id*,#{hook_arguments}},0}}}";

/// Ctrl+Q gets you back after going in. The key is bound on the tmux server (no prefix): detach in aca's sessions, and in
/// other sessions the key goes through to the program inside. agent-deck binds this key too, recognizing only sessions it
/// named (`agentdeck_*`) and swallowing the key in others — once an adopted session is renamed, it stops working there.
/// So its binding is replaced with one that recognizes both; a different binding of your own is left alone. The binding
/// isn't in tmux.conf: a server restart or agent-deck starting another session wipes it, so it's redone after creating a
/// session and before going into one. When agent-deck starts a session while you're already inside one (it binds once per
/// session it starts), none of those points come up: a hook is set so tmux switches it back the moment it binds. Going in
/// waits for this to finish: one list-keys to see the current state, and one tmux call for all the changes
pub fn bind_detach() {
    let Ok(keys) = run(&["list-keys"]) else { return };
    let mut cmds = detach_changes(&keys);
    // The hotkeys plugins want aren't in tmux.conf either: fill them in along with it
    if let Some(want) = crate::plugin::hotkey_binds() {
        cmds.extend(hotkey_changes(&keys, &want).0);
    }
    let _ = run(&cmds.join(&";".to_string()));
}

/// The commands Ctrl+Q needs. The hook is set every time (checking whether it's there costs as much as setting it); the
/// key is bound only if it isn't this version. The hook goes first: when one command in a chain fails, tmux skips the rest
fn detach_changes(keys: &str) -> Vec<Vec<String>> {
    let s = |x: &str| x.to_string();
    let again = format!("bind-key -n {DETACH_KEY} if-shell -F '{DETACH_IN}' detach-client 'send-keys {DETACH_KEY}'");
    let mut cmds = vec![vec![s("set-hook"), s("-g"), s(DETACH_HOOK), format!("if-shell -F \"{DECK_BOUND}\" \"{again}\"")]];
    if rebind(bound(keys, DETACH_KEY)) {
        let bind = ["bind-key", "-n", DETACH_KEY, "if-shell", "-F", DETACH_IN, "detach-client"];
        cmds.push(bind.into_iter().map(s).chain([format!("send-keys {DETACH_KEY}")]).collect());
    }
    cmds
}

/// Every hotkey command carries it: "aca bound this" is recognized by it. An `aca plugin pane …` you bound in your own
/// tmux.conf doesn't have it, so it won't be taken for aca's and unbound
pub const HOTKEY_MARK: &str = "--hotkey";
/// Hotkeys are only intercepted in aca's sessions
const HOTKEY_IN: &str = "#{?@aca_id,1,0}";
/// Bound in these key tables: normally (root), and while scrolling back (copy-mode; with vi keys, copy-mode-vi)
const HOTKEY_TABLES: [&str; 3] = ["root", "copy-mode", "copy-mode-vi"];

/// Plugin hotkeys (the manifest's `[[hotkeys]]`), done the same way as Ctrl+Q: bound on the tmux server, no prefix; in
/// aca's sessions the command runs, in other sessions the key goes through to the program inside. `want`: (key, shell
/// command to run on press — carrying [`HOTKEY_MARK`], with `#{…}` inside expanded by tmux). Keys aca bound before that
/// nobody wants now are unbound; keys someone else bound (your tmux.conf, tmux's defaults) are left alone. Returns the
/// keys that couldn't be bound in the normal key table. tmux not running: does nothing
pub fn bind_hotkeys(want: &[(String, String)]) -> Vec<String> {
    let Ok(keys) = run(&["list-keys"]) else { return vec![] };
    let (cmds, held) = hotkey_changes(&keys, want);
    if !cmds.is_empty() {
        // All in one call: a lone `;` between commands
        let _ = run(&cmds.join(&";".to_string()));
    }
    held
}

/// Which tmux commands turn the current key tables (`list-keys` output) into `want`; plus the keys in the normal key table
/// someone else holds
fn hotkey_changes(keys: &str, want: &[(String, String)]) -> (Vec<Vec<String>>, Vec<String>) {
    let s = |x: &str| x.to_string();
    let (mut cmds, mut held) = (vec![], vec![]);
    for table in HOTKEY_TABLES {
        let now: Vec<(&str, &str)> = bindings(keys, table).collect();
        for (k, _) in now.iter().filter(|(k, c)| c.contains(HOTKEY_MARK) && !want.iter().any(|(w, _)| same_key(w, k))) {
            cmds.push(vec![s("unbind-key"), s("-T"), s(table), s(k)]);
        }
        for (k, sh) in want {
            match now.iter().find(|(b, _)| same_key(b, k)) {
                Some((_, c)) if !c.contains(HOTKEY_MARK) => {
                    if table == "root" {
                        held.push(k.clone());
                    }
                }
                Some((_, c)) if plain(c).contains(&plain(sh)) => {}
                _ => {
                    let go = format!("run-shell -b {}", quote(sh));
                    let mut c = vec![s("bind-key"), s("-T"), s(table), k.clone(), s("if-shell"), s("-F"), s(HOTKEY_IN), go];
                    // Normally: not an aca session, so the key goes through as is. While scrolling back there's no "going through"
                    if table == "root" {
                        c.push(format!("send-keys {k}"));
                    }
                    cmds.push(c);
                }
            }
        }
    }
    (cmds, held)
}

/// Each binding in one key table from `list-keys`: (key, the command part)
fn bindings<'a>(keys: &'a str, table: &'a str) -> impl Iterator<Item = (&'a str, &'a str)> {
    keys.lines().filter_map(move |l| {
        let (t, rest) = l.split_once("-T")?.1.trim_start().split_once(char::is_whitespace)?;
        let (k, cmd) = rest.trim_start().split_once(char::is_whitespace)?;
        (t == table).then(|| (k, cmd.trim_start()))
    })
}

/// Whether two keys are the same: the order of modifiers doesn't matter. Debian 12's tmux 3.3a and Ubuntu 24.04's 3.4
/// print `C-M-y` as `M-C-y` in `list-keys` (newer tmux on the Mac prints it as written), so comparing literally would
/// recognize neither aca's own bindings nor yours
fn same_key(a: &str, b: &str) -> bool {
    key_parts(a) == key_parts(b)
}

/// (which modifiers C / M / S are held, the remaining key). `M--` is Alt plus minus: the last piece isn't split further
fn key_parts(k: &str) -> ([bool; 3], &str) {
    let mut mods = [false; 3];
    let mut rest = k;
    while rest.len() > 2 {
        let i = match rest.get(..2) {
            Some("C-") => 0,
            Some("M-") => 1,
            Some("S-") => 2,
            _ => break,
        };
        mods[i] = true;
        rest = &rest[2..];
    }
    (mods, rest)
}

/// Writes text as one argument of a tmux command (inside double quotes, `\` `"` `$` are escaped)
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        if matches!(c, '\\' | '"' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out + "\""
}

/// Strips quotes and escapes: `list-keys` prints commands quoted differently from how they were bound, so comparisons
/// ignore them
fn plain(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '\\' | '"' | '\'')).collect()
}

/// The command this key is bound to in the normal key table, from `list-keys` (the command part of
/// `bind-key [-r] -T root C-q <command> …`)
fn bound<'a>(keys: &'a str, key: &str) -> Option<&'a str> {
    bindings(keys, "root").find(|(k, _)| same_key(k, key)).map(|(_, cmd)| cmd)
}

/// Not bound, bound by agent-deck, bound by an older aca (condition changed) → bind; already this version, or bound by
/// someone else → leave alone
fn rebind(now: Option<&str>) -> bool {
    match now {
        None => true,
        Some(c) if c.contains(DETACH_IN) => false,
        Some(c) => c.contains("agentdeck_") || c.contains("@aca_id"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_titles_but_stay_addressable() {
        assert_eq!(safe_name("修登录 bug"), "修登录 bug");
        assert_eq!(safe_name("  fix v1.2:  auth\t\n"), "fix v1_2_ auth");
        assert_eq!(safe_name(""), "aca");
        assert_eq!(safe_name(&"长".repeat(80)).chars().count(), 48);
        let taken = vec!["a".to_string(), "a (2)".to_string()];
        assert_eq!(unique_name("a", &taken), "a (3)");
        assert_eq!(unique_name("b", &taken), "b");
    }

    #[test]
    fn hotkeys_are_bound_where_free_rebound_when_changed_and_unbound_when_nobody_wants_them() {
        let sh = |pane: &str, key: &str| {
            format!("'/bin/aca' plugin pane aca.copy {pane} {HOTKEY_MARK} {key} --session '#{{@aca_id}}' --client '#{{client_name}}'")
        };
        let want = vec![("C-]".to_string(), sh("pick", "C-]"))];
        // Nothing bound: one binding in each of the three key tables; in the normal one, non-aca sessions get the key passed
        // through
        let (cmds, held) = hotkey_changes("bind-key    -T root         M-.   next-window\n", &want);
        assert!(held.is_empty());
        assert_eq!(cmds.len(), 3);
        assert_eq!(cmds[0][..7], ["bind-key", "-T", "root", "C-]", "if-shell", "-F", HOTKEY_IN]);
        assert_eq!(cmds[0][7], format!("run-shell -b \"{}\"", sh("pick", "C-]")));
        assert_eq!((cmds[0][8].as_str(), cmds[1].len(), cmds[2][2].as_str()), ("send-keys C-]", 8, "copy-mode-vi"));
        // This is what tmux prints (quoted differently): already this binding, no need to bind again
        let line = |table: &str, key: &str, pane: &str| {
            format!(
                "bind-key    -T {table} {key}  if-shell -F \"#{{?@aca_id,1,0}}\" \"run-shell -b \\\"'/bin/aca' plugin pane aca.copy {pane} --hotkey {key} --session '#{{@aca_id}}' --client '#{{client_name}}'\\\"\" \"send-keys {key}\"\n"
            )
        };
        let all = |key: &str, pane: &str| HOTKEY_TABLES.map(|t| line(t, key, pane)).concat();
        assert_eq!(hotkey_changes(&all("C-]", "pick"), &want), (vec![], vec![]));
        // Pane to open changed: rebind. Key changed: unbind the old, bind the new. Nobody wants it any more: unbind all
        assert_eq!(hotkey_changes(&all("C-]", "old"), &want).0.len(), 3);
        let (cmds, _) = hotkey_changes(&all("M-y", "pick"), &want);
        assert_eq!(cmds.iter().filter(|c| c[0] == "unbind-key" && c[3] == "M-y").count(), 3);
        assert_eq!(cmds.iter().filter(|c| c[0] == "bind-key" && c[3] == "C-]").count(), 3);
        let (cmds, _) = hotkey_changes(&all("C-]", "pick"), &[]);
        assert_eq!(cmds, HOTKEY_TABLES.map(|t| vec!["unbind-key".to_string(), "-T".into(), t.into(), "C-]".into()]));
        // Your own binding (even one opening an aca pane): left alone, and the one in the normal key table is reported
        let yours =
            "bind-key -T root C-] run-shell -b \"aca plugin pane aca.note pad\"\nbind-key -T copy-mode-vi C-] send-keys -X cancel\n";
        let (cmds, held) = hotkey_changes(yours, &want);
        assert_eq!(held, ["C-]"]);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0][2], "copy-mode");
        assert!(hotkey_changes(yours, &[]).0.is_empty());
        assert_eq!(quote("a \"b\" $c \\"), "\"a \\\"b\\\" \\$c \\\\\"");
    }

    #[test]
    fn an_older_tmux_listing_the_modifiers_the_other_way_round_still_names_the_same_key() {
        assert!(same_key("C-M-y", "M-C-y") && same_key("C-q", "C-q") && same_key("M--", "M--"));
        assert!(!same_key("C-y", "M-y") && !same_key("C-M-y", "C-y") && !same_key("M--", "M-"));
        let sh = format!("'/bin/aca' next {HOTKEY_MARK} C-M-n --session '#{{@aca_id}}' --client '#{{client_name}}'");
        let want = vec![("C-M-n".to_string(), sh.clone())];
        // tmux 3.4 prints aca's C-M-n as M-C-n: recognized as aca's, neither unbound nor rebound
        let ours = HOTKEY_TABLES
            .map(|t| format!("bind-key -T {t} M-C-n if-shell -F \"#{{?@aca_id,1,0}}\" \"run-shell -b \\\"{sh}\\\"\" \"send-keys C-M-n\"\n"))
            .concat();
        assert_eq!(hotkey_changes(&ours, &want), (vec![], vec![]));
        // Yours on C-M-n is also printed as M-C-n: left alone and reported; the two scroll-back tables are free, so bind there
        let (cmds, held) = hotkey_changes("bind-key -T root M-C-n display-message mine\n", &want);
        assert_eq!(held, ["C-M-n"]);
        assert_eq!(cmds.iter().map(|c| c[2].as_str()).collect::<Vec<_>>(), ["copy-mode", "copy-mode-vi"]);
    }

    #[test]
    fn ctrl_q_is_taken_from_agent_deck_but_not_from_you() {
        let line = |cmd: &str| {
            format!("bind-key    -T root         M-.   next-window\nbind-key -r -T root         C-q                       {cmd}\n")
        };
        assert_eq!(bound(&line("detach-client"), "C-q"), Some("detach-client"));
        assert_eq!(bound("bind-key  -T root M-. next-window\n", "C-q"), None);
        assert!(rebind(None));
        // agent-deck's binding: only recognizes sessions it named
        let deck = line("if-shell -F \"#{m:agentdeck_*,#{session_name}}\" detach-client ''");
        assert!(rebind(bound(&deck, "C-q")));
        // Already this version: no need to bind again
        let ours = line(&format!("if-shell -F \"{DETACH_IN}\" detach-client \"send-keys C-q\""));
        assert!(!rebind(bound(&ours, "C-q")));
        // The version from before the condition changed: replace it
        assert!(rebind(bound(&line("if-shell -F \"#{@aca_id}\" detach-client"), "C-q")));
        // Your own binding
        assert!(!rebind(bound(&line("display-popup -E htop"), "C-q")));
        // All key tables are read: C-q in other tables doesn't count
        assert_eq!(bound("bind-key -T copy-mode C-q send-keys -X cancel\n", "C-q"), None);
        // The hook is set every time, first; if the key is already this version it isn't bound again
        let first = |cmds: &[Vec<String>]| cmds.iter().map(|c| c[0].clone()).collect::<Vec<_>>();
        assert_eq!(first(&detach_changes(&ours)), ["set-hook"]);
        assert_eq!(first(&detach_changes(&deck)), ["set-hook", "bind-key"]);
        assert_eq!(detach_changes(&deck)[1].last().map(String::as_str), Some("send-keys C-q"));
    }

    #[test]
    fn a_pane_title_may_contain_tabs_and_the_agent_pane_wins() {
        let out = "ab12\t\t$1\t标题\t%3\t100\tzsh\t0\t0\t\t5\t1\t0\t/p\t6\t2\tzsh\t\tshell\n\
                   ab12\t1\t$1\t标题\t%2\t99\tnode\t0\t1\t7\t5\t0\t0\t/p\t6\t2\tzsh\t\t✳ 带\t制表符\n\
                   \t\t$2\t别的\t%9\t1\tzsh\t1\t0\t\t5\t0\t0\t/\t\t1\tzsh\t\t\n";
        let v = parse_panes(out);
        assert_eq!(v.len(), 3);
        let a = agent_pane(&v, "ab12").unwrap();
        assert_eq!((a.pane.as_str(), a.title.as_str(), a.attached, a.last_attached), ("%2", "✳ 带\t制表符", 1, 7));
        assert!(v[2].dead && v[2].id.is_empty());
        // None marked @aca_agent: the pane with the lowest index
        let mut w = v.clone();
        w[1].agent = false;
        assert_eq!(agent_pane(&w, "ab12").unwrap().pane, "%2");
        assert!(agent_pane(&v, "nope").is_none());
    }

    #[test]
    fn a_pass_on_the_way_to_another_session_is_not_an_attach() {
        let row = |last: u64, active: u64, pass: &str| {
            let v = parse_panes(&format!("ab12\t1\t$1\t标题\t%2\t99\tnode\t0\t0\t{last}\t{active}\t0\t0\t/p\t6\t1\tzsh\t{pass}\t✳ 标题\n"));
            (v[0].last_attached, v[0].activity)
        };
        // Before the pass: attached at 40, touched at 45; passed at 50 and nobody came since: still 40, 45
        assert_eq!(row(50, 50, "40/45/50"), (40, 45));
        // Never attached (tmux gives empty)
        assert_eq!(row(50, 50, "/45/50"), (0, 45));
        // Attached again after the pass (60): go by what tmux says
        assert_eq!(row(60, 61, "40/45/50"), (60, 61));
        // Attaching to via failed, only the first half recorded; nothing recorded
        assert_eq!(row(50, 50, "40/45"), (50, 50));
        assert_eq!(row(50, 50, ""), (50, 50));
        // Pass through one, then the one to go into, in the same command
        let a = attach_via("$3", "$7").join(" ");
        assert!(a.contains("attach-session -t $3 ;") && a.ends_with("switch-client -t $7"), "{a}");
    }

    #[test]
    fn a_trailing_semicolon_is_not_a_tmux_separator() {
        assert_eq!(arg(";"), "\\;");
        assert_eq!(arg("echo hi;"), "echo hi\\;");
        assert_eq!(arg("a;b"), "a;b");
    }

    #[test]
    fn many_panes_are_captured_by_one_tmux_and_a_pane_that_is_gone_costs_one_more() {
        let words = |a: &[String]| a.join(" ");
        assert_eq!(
            words(&capture_args(&["%1", "%2"], true, "CUT")),
            "capture-pane -p -e -t %1 ; display-message -p -t %1 CUT ; capture-pane -p -e -t %2 ; display-message -p -t %2 CUT"
        );
        assert_eq!(words(&capture_args(&["%1"], false, "CUT")), "capture-pane -p -t %1 ; display-message -p -t %1 CUT");
        // A chunk is exactly what capturing that pane alone prints; an empty screen is a chunk too; a last half-chunk that never
        // got its mark doesn't count; other marks are just a line of text on the screen
        assert_eq!(split_marked("a\n  b  \n\nCUT\n\nCUT\nCUT-old\nCUT\n半段\n", "CUT"), ["a\n  b  \n\n", "\n", "CUT-old\n"]);
        assert_eq!(plain_lines("a  \n\n b\n\n\n"), ["a", "", " b"]);

        // A fake tmux: runs commands one by one and stops at a pane it can't capture, skipping the rest
        let screen = |p: &str| match p {
            "%1" => Some("一\n"),
            "%3" => Some("\n"),
            "%4" => Some("四\n"),
            _ => None,
        };
        let fake = |calls: &mut Vec<usize>, args: &[String]| {
            calls.push(args.iter().filter(|a| *a == "capture-pane").count());
            let mut out = String::new();
            for c in args.split(|a| a == ";") {
                match (c[0].as_str(), c.last().unwrap().as_str()) {
                    ("capture-pane", p) => match screen(p) {
                        Some(s) => out.push_str(s),
                        None => break,
                    },
                    (_, mark) => out.push_str(&format!("{mark}\n")),
                }
            }
            out
        };
        let mut calls = vec![];
        assert_eq!(capture_with(&["%1", "%2", "%3", "%4"], false, "CUT", |a| fake(&mut calls, a)), ["一\n", "", "\n", "四\n"]);
        // All there: one process; %2 gone: the second half of the call before it was wasted, so go again from %3
        assert_eq!(calls, [4, 2]);
        let mut calls = vec![];
        assert_eq!(capture_with(&["%1", "%3", "%4"], false, "CUT", |a| fake(&mut calls, a)).len(), 3);
        assert_eq!(calls, [3]);
        // No pane can be captured (tmux gone): one call per pane, so it can't loop forever; no panes, no tmux
        let mut calls = vec![];
        assert_eq!(capture_with(&["%8", "%9"], false, "CUT", |a| fake(&mut calls, a)), ["", ""]);
        assert_eq!(calls, [2, 1]);
        assert!(capture_with(&[], false, "CUT", |_| panic!("起了 tmux")).is_empty());
    }
}
