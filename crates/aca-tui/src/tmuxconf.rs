//! `aca tmux install | uninstall`:aca 在 tmux 里要的几样 —— 会话切换器的两个键、状态栏上谁在等你 —— 写在 aca 自己的一份
//! 配置里(`~/.config/aca/tmux.conf`),你的 tmux.conf 里只多两行:一行注释、一行 `source-file -q`。
//!
//! - aca 那份归 aca:每次 install 照新的重写(install.sh 升级时也重写一遍;没跑 install.sh 的升级,打开界面时重写,见 `refresh`),别在里面改。要换键,在你的 tmux.conf 里
//!   source 它的那一行后面自己 unbind / bind
//! - 你的 tmux.conf 只加、只删那两行,别的一个字节不动;放在 TPM 那一段前面(它照规矩在最后),没有 TPM 就接在最后 ——
//!   状态栏那一段是接在你的 status-right 后面的,要排在你设 status-right 的那几行后面
//! - 是链接(dotfiles 管的)就写到它指着的那个文件,不把链接换成普通文件
//! - `-q`:aca 那份不在了(删了 aca)也不报错
//! - 开着的 tmux 马上变:install 时 source 一遍 aca 那份(状态栏那段已经接过的不再接);uninstall 时撤掉 aca 绑的键和那一段
//! - 设了 `ACA_HOME`(测试)时「你的 tmux.conf」是 `$ACA_HOME/tmux.conf`,碰不到真的
//!
//! `aca tmux key`:aca 自己绑在 tmux 上、你能换的键 —— 现在就一个,进下一个在等你的(`aca next`)。它不在上面那份配置里:
//! 和 Ctrl+Q、插件的热键一个办法,aca 直接绑在 tmux 服务上(`plugin::bind_hotkeys`),哪个键记在 config.toml 的 `[keys] next`

use std::path::{Path, PathBuf};

use aca_core::paths;
use aca_core::{t, tr};

/// 加在你的 tmux.conf 里那行注释的开头(卸的时候认它)
const MARK: &str = "# aca:";
/// aca 那份绑的命令都带着它(你自己手写的不带):uninstall 只撤带它的
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

/// aca 自己那份
fn ours() -> PathBuf {
    paths::config_dir().join("tmux.conf")
}

/// 你的 tmux.conf:`~/.tmux.conf`;没有它、有 `~/.config/tmux/tmux.conf`(XDG)就是那个
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

/// 给人看、写进 source-file 的路径:家目录下的写成 ~/…(tmux 自己会展开)
fn shown(p: &Path) -> String {
    let home = paths::home();
    match p.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => p.display().to_string(),
    }
}

/// 你的 tmux.conf 里加的那一行
fn source_line(p: &Path) -> String {
    let s = shown(p);
    // 带空格的:引起来,不用 ~(引号里不展开)
    if s.contains(char::is_whitespace) { format!("source-file -q \"{}\"", p.display()) } else { format!("source-file -q {s}") }
}

fn is_ours(line: &str, p: &Path) -> bool {
    let l = line.trim();
    l.starts_with("source-file") && (l.contains(&shown(p)) || l.contains(&p.display().to_string()))
}

/// 状态栏上接的那一段(uninstall 照原样从 status-right 里摘掉)
fn segment(aca: &str) -> String {
    format!(" #({aca} tmux-status #{{@aca_id}})")
}

/// aca 那份的全文。`aca` = 这个 aca 的完整路径
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

/// 开着的 tmux 里那一段接了不止一遍的(早先那份的判断在自己设了 status-right 的会话里看不到全局的,每装一次多接一遍):
/// 只留头一处。原来有几遍;没接重 = None
fn dedupe(aca: &str) -> Option<usize> {
    use aca_core::tmux::run;
    let right = run(&["show-options", "-gv", "status-right"]).unwrap_or_default();
    let (right, seg) = (right.trim_end_matches('\n'), segment(aca));
    let n = right.matches(&seg).count();
    let (head, rest) = right.split_at(right.find(&seg)? + seg.len());
    (n > 1 && run(&["set-option", "-g", "status-right", &format!("{head}{}", rest.replace(&seg, ""))]).is_ok()).then_some(n)
}

/// 加上那两行,返回新的全文和 source-file 那一行是第几行。已经有了 = None
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
    // TPM 那一行(run '~/.tmux/plugins/tpm/tpm')连同它上面紧挨着的注释
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

/// 拿掉那两行(和加的时候垫的空行)。没有 = None
fn remove(text: &str, p: &Path) -> Option<String> {
    let mut lines: Vec<&str> = text.lines().collect();
    let i = lines.iter().position(|l| is_ours(l, p))?;
    let start = if i > 0 && lines[i - 1].trim_start().starts_with(MARK) { i - 1 } else { i };
    lines.drain(start..=i);
    // 两边都是空行(或者到了末尾)就收掉一个,别留下两个空行
    if start > 0 && lines[start - 1].trim().is_empty() && lines.get(start).is_none_or(|l| l.trim().is_empty()) {
        lines.remove(start - 1);
    }
    Some(if lines.is_empty() { String::new() } else { lines.join("\n") + "\n" })
}

/// 你自己手写的 aca 那几样(没注释掉的):第几行
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

/// 写你的 tmux.conf:是链接就写它指着的那个
fn write_theirs(path: &Path, text: &str) -> Res<()> {
    let real = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    paths::write_atomic(&real, text).map_err(|e| tr!("无法写入 {}:{e}", "can't write {}: {e}", shown(path)))
}

type Res<T> = Result<T, String>;

/// tmux 服务在不在跑(不在就不用管开着的那份)
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

/// 界面打开时(^L 重启成新版也是):aca 那份装过、写的就是这个程序,内容却和这一版要写的不一样(升级了、换了界面语言)
/// → 照这一版重写,开着的 tmux 也 source 一遍。brew 之类的升级不跑 install.sh,靠它跟上。
/// 写的是别的 aca(工作树里编的、另一种装法装的)就不动:免得开一下临时编的程序,tmux 的键就指到它上面
pub fn refresh() {
    let (aca, file) = (aca_core::agent::self_exe(), ours());
    let Ok(old) = std::fs::read_to_string(&file) else { return };
    let new = content(&aca);
    if old != new && stale(&old, &aca) && paths::write_atomic(&file, &new).is_ok() && live() {
        let _ = aca_core::tmux::run(&["source-file", &file.display().to_string()]);
    }
}

/// aca 那份里用的就是这个程序(`"<aca> ui --client`、`#(<aca> tmux-status`):前面带着引号、括号,别的路径以它结尾的不算
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

/// 开着的 tmux 里撤掉 aca 那份绑的键(只撤带 OURS_TAG 的,你自己绑的不动)和状态栏上接的那一段。撤了什么 = true
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

/// `aca tmux key [next [<键> | --none]]`:看 / 换进下一个在等你的那个键。换了马上绑上
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

/// 进下一个在等你的那个键现在是哪个、绑上没有。顺手让 tmux 上绑的跟配置一致(同 `aca plugin hotkey`)
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

/// tmux 的键名写成键盘上的样子:`C-M-n` → `Ctrl+Alt+N(C-M-n)`;认不出的原样
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
        // 没有 TPM:接在最后;没有文件:就这两行
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
        // 工作树里编的、路径是它结尾的别的程序:不动
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
