//! `aca tmux install | uninstall`: the few things aca needs in tmux (the session switcher's two keys, which sessions need you
//! in the status bar) are written in aca's own config file (`~/.config/aca/tmux.conf`); your tmux.conf gains only two lines:
//! a comment and a `source-file -q`.
//!
//! - aca's file belongs to aca: every install rewrites it from scratch (install.sh rewrites it on upgrade too; for an upgrade
//!   that doesn't run install.sh, it's rewritten when the interface opens, see `refresh`); don't edit it. To change a key,
//!   unbind / bind it yourself in your tmux.conf after the line that sources it
//! - in your tmux.conf only those two lines are added or removed, no other byte is touched; they go before the TPM section
//!   (which by convention comes last), or at the end without TPM: the status bar segment is appended to your status-right,
//!   so it must come after the lines where you set status-right
//! - a symlink (managed by dotfiles) is written through to the file it points at; the link isn't replaced with a regular file
//! - `-q`: no error once aca's file is gone (aca removed)
//! - a running tmux changes right away: install sources aca's file once (a status bar segment already appended isn't
//!   appended again); uninstall removes the keys aca bound and that segment
//! - with `ACA_HOME` set (tests), "your tmux.conf" is `$ACA_HOME/tmux.conf`, so the real one is never touched
//!
//! `aca tmux key`: the keys aca binds on tmux itself that you can change: just one for now, go to the next session that
//! needs you (`aca next`). It isn't in the file above: like Ctrl+Q and plugin hotkeys, aca binds it directly on the tmux
//! server (`plugin::bind_hotkeys`); which key is kept in config.toml's `[keys] next`

use std::path::{Path, PathBuf};

use aca_core::paths;
use aca_core::{t, tr};

/// How the comment line added to your tmux.conf starts (uninstall recognizes it by this)
const MARK: &str = "# aca:";
/// Every command aca's file binds carries it (ones you wrote yourself don't): uninstall only removes the ones carrying it
const OURS_TAG: &str = "#{socket_path}";

pub fn run(args: &[String]) -> i32 {
    let r = match args.first().map(String::as_str) {
        None | Some("status") => status(),
        Some("install") => install(),
        Some("uninstall") => uninstall(),
        Some("key") => key(&args[1..]),
        Some(other) => {
            Err(tr!("aca tmux 不支持 {other}(可用:install / uninstall / key)", "aca tmux has no {other} (install / uninstall / key)"))
        }
    };
    match r {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// aca's own file
fn ours() -> PathBuf {
    paths::config_dir().join("tmux.conf")
}

/// Your tmux.conf: `~/.tmux.conf`; if it doesn't exist and `~/.config/tmux/tmux.conf` (XDG) does, that one
fn theirs() -> PathBuf {
    if let Some(h) = std::env::var_os("ACA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h).join("tmux.conf");
    }
    let dot = paths::home().join(".tmux.conf");
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::home().join(".config"))
        .join("tmux/tmux.conf");
    if !dot.exists() && xdg.exists() { xdg } else { dot }
}

/// The path shown to people and written into source-file: under the home directory it's written ~/… (tmux expands it itself)
fn shown(p: &Path) -> String {
    let home = paths::home();
    match p.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => p.display().to_string(),
    }
}

/// The line added to your tmux.conf
fn source_line(p: &Path) -> String {
    let s = shown(p);
    // With spaces: quote it and don't use ~ (not expanded inside quotes)
    if s.contains(char::is_whitespace) { format!("source-file -q \"{}\"", p.display()) } else { format!("source-file -q {s}") }
}

fn is_ours(line: &str, p: &Path) -> bool {
    let l = line.trim();
    l.starts_with("source-file") && (l.contains(&shown(p)) || l.contains(&p.display().to_string()))
}

/// The segment appended to the status bar (uninstall takes it out of status-right exactly as written)
fn segment(aca: &str) -> String {
    format!(" #({aca} tmux-status #{{@aca_id}})")
}

/// The full text of aca's file. `aca` = this aca's full path
fn content(aca: &str) -> String {
    let popup = |args: &str, follow: &str| {
        format!(
            "run-shell -b 'tmux -S \"{OURS_TAG}\" display-popup {args} -c \"#{{client_name}}\" \"{aca} ui --client #{{client_name}}{follow}\"'"
        )
    };
    let full = popup("-B -E -w 100% -h 100%", "");
    let side = popup("-E -x R -y 0 -w 56 -h 100%", " --follow");
    let head = t!(
        "\
# aca 的 tmux 设置。此文件由 aca tmux install 生成,升级 aca 时会重新生成,请勿在此修改 ——
# 如需更换按键,在你的 tmux.conf 中 source 本文件的那一行之后自行 unbind / bind。卸载:aca tmux uninstall

# 会话切换器(aca help keys「作为切换器使用」):prefix + a 全屏打开;不按前缀直接按 Ctrl+\\ 在右侧打开(宽 56 列,
# 旁边的窗格跟随高亮切换会话)。窗口不足 120 列(如手机)时 Ctrl+\\ 也全屏打开:tmux 无法打开比窗口更宽的弹窗。
# 回车只切换按键所在的客户端(手机和桌面互不影响)",
        "\
# aca's tmux settings. Written by aca tmux install and rewritten when aca is upgraded: don't edit here —
# to change a key, unbind / bind after the line in your tmux.conf that sources this. Not wanted: aca tmux uninstall

# Session switcher (aca help keys, \"As a switcher\"): prefix + a opens it full screen; Ctrl+\\ without the prefix
# opens it on the right (56 columns; the session beside it follows the highlight). On a window under 120 columns
# (a phone) Ctrl+\\ opens it full screen too: tmux can't open a popup wider than the window.
# Enter switches only the client you pressed it on (phone and desktop each their own)"
    );
    let status = t!(
        "# 状态栏:显示待处理的会话(aca help tmux-status),追加在你的 status-right 之后。已追加的不会重复追加,重新加载配置也不会出现两份。\n\
         # 判断依据是全局值(show-options -g):在自己设置了 status-right 的会话中,#{status-right} 取到的是该会话的值",
        "# Status bar: which sessions wait for you (aca help tmux-status), after your status-right. Not added twice:\n\
         # reloading the config doesn't append it again. It looks at the global value (show-options -g): in a session\n\
         # with a status-right of its own, #{status-right} is that session's"
    );
    format!(
        "{head}\n\
         bind a {full}\n\
         bind -n 'C-\\' if-shell -F '#{{e|>=:#{{client_width}},120}}' {{\n  {side}\n}} {{\n  {full}\n}}\n\
         \n\
         {status}\n\
         if-shell 'tmux -S \"{OURS_TAG}\" show-options -gv status-right | grep -qv tmux-status' {{\n  \
         set -ag status-right \"{}\"\n\
         }}\n",
        segment(aca)
    )
}

/// The running tmux has that segment more than once (an earlier file's check couldn't see the global value in a session with
/// its own status-right, so every install appended one more): keep only the first. Returns how many there were; no
/// duplicate = None
fn dedupe(aca: &str) -> Option<usize> {
    use aca_core::tmux::run;
    let right = run(&["show-options", "-gv", "status-right"]).unwrap_or_default();
    let (right, seg) = (right.trim_end_matches('\n'), segment(aca));
    let n = right.matches(&seg).count();
    let (head, rest) = right.split_at(right.find(&seg)? + seg.len());
    (n > 1 && run(&["set-option", "-g", "status-right", &format!("{head}{}", rest.replace(&seg, ""))]).is_ok()).then_some(n)
}

/// Add the two lines; returns the new full text and the line number of the source-file line. Already there = None
fn add(text: &str, p: &Path) -> Option<(String, usize)> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.iter().any(|l| is_ours(l, p)) {
        return None;
    }
    let note = format!(
        "{MARK}{}",
        t!(
            "会话切换器(prefix + a、Ctrl+\\)和状态栏的待处理提示都在 aca 的配置文件中。卸载:aca tmux uninstall",
            " the session switcher (prefix + a, Ctrl+\\) and who waits for you in the status bar live in aca's own file. To remove: aca tmux uninstall"
        )
    );
    let block = [note, source_line(p)];
    // The TPM line (run '~/.tmux/plugins/tpm/tpm') together with the comments right above it
    let at = lines.iter().rposition(|l| l.trim_start().starts_with("run") && l.contains("tpm/tpm")).map(|mut j| {
        while j > 0 && lines[j - 1].trim_start().starts_with('#') {
            j -= 1;
        }
        j
    });
    let blank_before = |j: usize| j > 0 && !lines[j - 1].trim().is_empty();
    let mut out: Vec<String> = vec![];
    let line_no;
    match at {
        Some(j) => {
            out.extend(lines[..j].iter().map(|s| s.to_string()));
            if blank_before(j) {
                out.push(String::new());
            }
            out.extend(block);
            line_no = out.len();
            out.push(String::new());
            out.extend(lines[j..].iter().map(|s| s.to_string()));
        }
        None => {
            out.extend(lines.iter().map(|s| s.to_string()));
            if blank_before(lines.len()) {
                out.push(String::new());
            }
            out.extend(block);
            line_no = out.len();
        }
    }
    Some((out.join("\n") + "\n", line_no))
}

/// Remove the two lines (and the blank line padded in when adding them). Not there = None
fn remove(text: &str, p: &Path) -> Option<String> {
    let mut lines: Vec<&str> = text.lines().collect();
    let i = lines.iter().position(|l| is_ours(l, p))?;
    let start = if i > 0 && lines[i - 1].trim_start().starts_with(MARK) { i - 1 } else { i };
    lines.drain(start..=i);
    // A blank line (or the end) on both sides: drop one, so no two blank lines are left
    if start > 0 && lines[start - 1].trim().is_empty() && lines.get(start).is_none_or(|l| l.trim().is_empty()) {
        lines.remove(start - 1);
    }
    Some(if lines.is_empty() { String::new() } else { lines.join("\n") + "\n" })
}

/// The aca lines you wrote yourself (not commented out): their line numbers
fn handwritten(text: &str, p: &Path) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !t.starts_with('#') && !is_ours(l, p) && t.contains("aca") && (t.contains(" ui --client") || t.contains("tmux-status"))
        })
        .map(|(i, _)| i + 1)
        .collect()
}

/// Write your tmux.conf: if it's a symlink, write the file it points at
fn write_theirs(path: &Path, text: &str) -> Res<()> {
    let real = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    paths::write_atomic(&real, text).map_err(|e| tr!("无法写入 {}:{e}", "can't write {}: {e}", shown(path)))
}

type Res<T> = Result<T, String>;

/// Whether the tmux server is running (if not, there's no running config to care about)
fn live() -> bool {
    aca_core::tmux::run(&["list-sessions"]).is_ok()
}

fn install() -> Res<String> {
    let aca = aca_core::agent::self_exe();
    let (file, conf) = (ours(), theirs());
    paths::write_atomic(&file, &content(&aca)).map_err(|e| tr!("无法写入 {}:{e}", "can't write {}: {e}", shown(&file)))?;
    let mut say = vec![tr!(
        "已写入 {}:切换器 prefix + a、Ctrl+\\,状态栏待处理提示",
        "wrote {}: the switcher on prefix + a and Ctrl+\\, who waits for you in the status bar",
        shown(&file)
    )];
    let text = std::fs::read_to_string(&conf).unwrap_or_default();
    match add(&text, &file) {
        Some((new, n)) => {
            write_theirs(&conf, &new)?;
            say.push(tr!("已在 {} 第 {n} 行添加:{}", "{} line {n} now has: {}", shown(&conf), source_line(&file)));
        }
        None => say.push(tr!("{} 中已有 source 它的行", "{} already sources it", shown(&conf))),
    }
    if live() {
        match aca_core::tmux::run(&["source-file", &file.display().to_string()]) {
            Ok(_) => say.push(t!("运行中的 tmux 已生效", "the running tmux uses it now").into()),
            Err(e) => say.push(tr!("运行中的 tmux 未能更新:{e}", "the running tmux didn't take it: {e}")),
        }
        if let Some(n) = dedupe(&aca) {
            say.push(tr!(
                "状态栏待处理提示原先重复了 {n} 次,已去重",
                "the who-waits-for-you segment was in the status bar {n} times: kept one"
            ));
        }
    }
    let mine = handwritten(&std::fs::read_to_string(&conf).unwrap_or_default(), &file);
    if !mine.is_empty() {
        say.push(tr!(
            "注意:{} 第 {} 行是你自己添加的 aca 按键 / 状态栏,与 aca 的配置重复(当前生效的是 aca 的配置),可以删除",
            "note: lines {1} of {0} are aca keys / status you wrote yourself and duplicate aca's file (which comes later and wins): delete them",
            shown(&conf),
            lines_text(&mine)
        ));
    }
    Ok(say.join("\n"))
}

/// When the interface opens (also after ^L restarts into a new version): aca's file is installed and names this very program,
/// but differs from what this version would write (upgraded, interface language changed)
/// → rewrite it per this version, and source it in the running tmux too. Upgrades like brew don't run install.sh; this keeps up.
/// If it names another aca (built in a worktree, installed another way), leave it: otherwise just running a throwaway build
/// once would point tmux's keys at it
pub fn refresh() {
    let (aca, file) = (aca_core::agent::self_exe(), ours());
    let Ok(old) = std::fs::read_to_string(&file) else { return };
    let new = content(&aca);
    if old != new && stale(&old, &aca) && paths::write_atomic(&file, &new).is_ok() && live() {
        let _ = aca_core::tmux::run(&["source-file", &file.display().to_string()]);
    }
}

/// aca's file uses this very program (`"<aca> ui --client`, `#(<aca> tmux-status`): with the quote / parenthesis before it, so
/// another path that merely ends with it doesn't count
fn stale(old: &str, aca: &str) -> bool {
    old.contains(&format!("\"{aca} ")) || old.contains(&format!("#({aca} "))
}

fn uninstall() -> Res<String> {
    let (file, conf) = (ours(), theirs());
    let mut say = vec![];
    let text = std::fs::read_to_string(&conf).unwrap_or_default();
    match remove(&text, &file) {
        Some(new) => {
            write_theirs(&conf, &new)?;
            say.push(tr!("已从 {} 中移除 source aca 配置的两行", "removed the two lines sourcing aca's file from {}", shown(&conf)));
        }
        None => say.push(tr!("{} 中没有 source aca 配置的行", "{} doesn't source aca's file", shown(&conf))),
    }
    if file.exists() {
        std::fs::remove_file(&file).map_err(|e| tr!("无法删除 {}:{e}", "can't delete {}: {e}", shown(&file)))?;
        say.push(tr!("已删除 {}", "deleted {}", shown(&file)));
    }
    if live() {
        let undone = unapply(&aca_core::agent::self_exe());
        if undone {
            say.push(
                t!("运行中的 tmux 已同时移除 aca 的按键和状态栏", "aca's keys and status segment are gone from the running tmux too")
                    .into(),
            );
        }
    }
    Ok(say.join("\n"))
}

/// In the running tmux, remove the keys aca's file bound (only those with OURS_TAG; ones you bound yourself stay) and the
/// segment appended to the status bar. Removed anything = true
fn unapply(aca: &str) -> bool {
    use aca_core::tmux::run;
    let mut any = false;
    for (table, key) in [("prefix", "a"), ("root", "C-\\")] {
        let keys = run(&["list-keys", "-T", table]).unwrap_or_default();
        let bound = keys.lines().any(|l| {
            let mut w = l.split_whitespace().skip_while(|w| *w != "-T").skip(2);
            let k = w.next().unwrap_or("");
            (k == key || (key != "a" && k.starts_with("C-\\"))) && l.contains(OURS_TAG) && l.contains(" ui --client")
        });
        if bound && run(&["unbind-key", "-T", table, key]).is_ok() {
            any = true;
        }
    }
    let right = run(&["show-options", "-gv", "status-right"]).unwrap_or_default();
    let right = right.trim_end_matches('\n');
    if right.contains(&segment(aca)) && run(&["set-option", "-g", "status-right", &right.replace(&segment(aca), "")]).is_ok() {
        any = true;
    }
    any
}

fn status() -> Res<String> {
    let (file, conf) = (ours(), theirs());
    let text = std::fs::read_to_string(&conf).unwrap_or_default();
    let on = text.lines().any(|l| is_ours(l, &file));
    let mut say = vec![if on && file.exists() {
        tr!("已安装:{} 引用了 {}", "installed: {} sources {}", shown(&conf), shown(&file))
    } else if on {
        tr!(
            "{} 中有 source 行,但 {} 已不存在:运行 aca tmux install 重新生成",
            "{} sources {}, which is gone: aca tmux install writes it again",
            shown(&conf),
            shown(&file)
        )
    } else {
        tr!(
            "未安装。运行 aca tmux install 会把切换器按键(prefix + a、Ctrl+\\)和状态栏待处理提示写入 {},并在 {} 中添加一行 source-file",
            "not installed. aca tmux install puts the switcher keys (prefix + a, Ctrl+\\) and who waits for you in the status bar into {}, and adds one source-file line to {}",
            shown(&file),
            shown(&conf)
        )
    }];
    let mine = handwritten(&text, &file);
    if !mine.is_empty() {
        say.push(tr!(
            "{} 第 {} 行是你自己添加的 aca 按键 / 状态栏",
            "lines {1} of {0} are aca keys / status you wrote yourself",
            shown(&conf),
            lines_text(&mine)
        ));
    }
    say.extend(next_key());
    Ok(say.join("\n"))
}

/// `aca tmux key [next [<key> | --none]]`: show / change the key that goes to the next session that needs you. A new key is
/// bound right away
fn key(args: &[String]) -> Res<String> {
    match args {
        [] => {}
        [which, rest @ ..] if which == "next" => {
            if let Some(k) = rest.first() {
                let k = if k == "--none" { "" } else { k.as_str() };
                if !k.is_empty() {
                    aca_core::plugin::hotkey_ok(k)?;
                }
                crate::config::set("keys", "next", toml::Value::String(k.into()))?;
            }
        }
        [other, ..] => return Err(tr!("aca tmux key 不支持 {other}(仅支持 next)", "aca tmux key has no {other} (only next)")),
    }
    Ok(next_key().join("\n"))
}

/// Which key goes to the next session that needs you, and whether it's bound. Also brings tmux's bindings in line with the
/// config (like `aca plugin hotkey`)
fn next_key() -> Vec<String> {
    let notes = aca_core::plugin::bind_hotkeys();
    let mut say = vec![match aca_core::config::load().keys.next_key() {
        Ok(Some(k)) => tr!(
            "跳到下一个待处理会话:在会话中按 {}(aca next;更换:aca tmux key next <键>,取消:--none)",
            "the next one waiting: press {} inside a session (aca next; another key: aca tmux key next <key>, none: --none)",
            human(&k)
        ),
        _ => t!(
            "跳到下一个待处理会话:未绑定按键(用 aca tmux key next <键> 绑定;也可直接运行 aca next)",
            "the next one waiting: no key (aca tmux key next <key> binds one; aca next still works typed)"
        )
        .into(),
    }];
    say.extend(notes.into_iter().filter(|(who, _)| who == aca_core::plugin::OWN_KEYS).map(|(_, n)| format!("⚠ {n}")));
    say
}

/// Write a tmux key name the way it looks on the keyboard: `C-M-n` → `Ctrl+Alt+N(C-M-n)`; unrecognized ones as they are
fn human(k: &str) -> String {
    let (mods, last) = match k {
        _ if k.starts_with("C-M-") => ("Ctrl+Alt+", &k[4..]),
        _ if k.starts_with("M-") => ("Alt+", &k[2..]),
        _ if k.starts_with("C-") => ("Ctrl+", &k[2..]),
        _ => return k.to_string(),
    };
    format!("{mods}{}({k})", last.to_uppercase())
}

fn lines_text(ns: &[usize]) -> String {
    ns.iter().map(usize::to_string).collect::<Vec<_>>().join(t!("、", ", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> PathBuf {
        PathBuf::from("/x/aca conf/tmux.conf")
    }

    #[test]
    fn the_two_lines_go_before_tpm_and_come_out_leaving_the_file_as_it_was() {
        let before =
            "set -g status-right \"MINE\"\nrun-shell ~/note.tmux\n\n# ====\n#  初始化 TPM\n# ====\nrun '~/.tmux/plugins/tpm/tpm'\n";
        let (with, n) = add(before, &p()).unwrap();
        let lines: Vec<&str> = with.lines().collect();
        assert!(lines[n - 2].starts_with(MARK) && lines[n - 1] == "source-file -q \"/x/aca conf/tmux.conf\"", "{with}");
        assert!(with.ends_with("\n\n# ====\n#  初始化 TPM\n# ====\nrun '~/.tmux/plugins/tpm/tpm'\n"), "{with}");
        assert!(with.find("MINE").unwrap() < with.find("source-file").unwrap());
        assert_eq!(add(&with, &p()), None);
        assert_eq!(remove(&with, &p()).as_deref(), Some(before));
        assert_eq!(remove(before, &p()), None);
        // No TPM: appended at the end; no file: just these two lines
        let plain = "set -g mouse on\n";
        let (with, _) = add(plain, &p()).unwrap();
        assert!(with.starts_with("set -g mouse on\n\n# aca:") && with.ends_with("tmux.conf\"\n"), "{with}");
        assert_eq!(remove(&with, &p()).as_deref(), Some(plain));
        let (with, n) = add("", &p()).unwrap();
        assert_eq!((with.lines().count(), n), (2, 2));
        assert_eq!(remove(&with, &p()).as_deref(), Some(""));
    }

    #[test]
    fn lines_you_wrote_for_aca_yourself_are_found_and_comments_are_not() {
        let text = "# bind a run-shell 'aca ui --client x'\nbind a run-shell 'tmux display-popup \"aca ui --client #{client_name}\"'\nset -g mouse on\n\
                    set -ag status-right \"#(aca tmux-status)\"\nsource-file -q \"/x/aca conf/tmux.conf\"\n";
        assert_eq!(handwritten(text, &p()), [2, 4]);
    }

    #[test]
    fn only_the_program_that_wrote_aca_s_file_rewrites_it() {
        let old = content("/opt/homebrew/bin/aca");
        assert!(stale(&old, "/opt/homebrew/bin/aca"));
        // One built in a worktree, another program whose path ends with it: left alone
        assert!(!stale(&old, "/x/target/debug/aca") && !stale(&old, "/homebrew/bin/aca") && !stale(&old, "aca"));
    }

    #[test]
    fn a_tmux_key_name_is_shown_the_way_it_is_on_the_keyboard() {
        assert_eq!(human("C-M-n"), "Ctrl+Alt+N(C-M-n)");
        assert_eq!((human("M-y"), human("C-]"), human("F5")), ("Alt+Y(M-y)".into(), "Ctrl+](C-])".into(), "F5".into()));
    }

    #[test]
    fn our_file_binds_both_keys_with_a_tag_and_appends_the_segment_once() {
        let c = content("/bin/aca");
        assert!(c.contains("bind a run-shell -b 'tmux -S \"#{socket_path}\" display-popup -B -E -w 100% -h 100%"), "{c}");
        assert!(c.contains("bind -n 'C-\\' if-shell -F '#{e|>=:#{client_width},120}' {\n  run-shell"), "{c}");
        assert!(c.contains("\"/bin/aca ui --client #{client_name} --follow\""), "{c}");
        assert!(
            c.contains(
                "if-shell 'tmux -S \"#{socket_path}\" show-options -gv status-right | grep -qv tmux-status' {\n  \
                 set -ag status-right \" #(/bin/aca tmux-status #{@aca_id})\"\n}\n"
            ),
            "{c}"
        );
    }
}
