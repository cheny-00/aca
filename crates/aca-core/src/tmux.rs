//! tmux 这一层:会话是 tmux 持有的,aca 只在上面记账。
//!
//! 认会话不靠名字:建的时候在 tmux 会话上记 `@aca_id`,agent 那一格记 `@aca_agent`。
//! 所以**会话名可以就是标题**,随便改(Moshi 这类客户端的列表里看到的就是它),
//! 发命令时一律用 tmux 自己的 `$会话号` / `%窗格号`,名字里有什么字符都不怕。
//!
//! `ACA_TMUX_SOCKET` 换一个 tmux 服务(`tmux -L`):测试只连假会话的那个,动不到真的。

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

/// 跑一条,成了给标准输出,不成给 tmux 说的那句
pub fn run<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Res<String> {
    let o = cmd().args(args).output().map_err(|e| tr!("tmux 起不来:{e}", "can't run tmux: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// tmux 把单独的 `;` 和词尾的 `;` 当成命令分隔;交给它的参数里有的话转义掉
pub fn arg(a: &str) -> String {
    match a.strip_suffix(';') {
        Some(head) => format!("{head}\\;"),
        None => a.to_string(),
    }
}

/// 一个窗格,连它所在会话的几样
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pane {
    /// `@aca_id`:空 = 不是 aca 建的
    pub id: String,
    /// `@aca_agent`:agent 在的那一格
    pub agent: bool,
    /// tmux 的会话号(`$3`),发命令用
    pub sid: String,
    pub session: String,
    pub pane: String,
    pub pid: u32,
    pub cmd: String,
    pub dead: bool,
    pub attached: u32,
    pub last_attached: u64,
    pub activity: u64,
    pub window: u32,
    pub index: u32,
    pub path: String,
    /// 这个窗口最后有输出的时刻(会话的 `activity` 是最后有人动的时刻,不跟输出走)
    pub win_activity: u64,
    /// 这个窗口里有几格(>1 = 分了屏)和窗口名
    pub win_panes: u32,
    pub win_name: String,
    /// 程序自己写的窗格标题(Claude 干活时前面带一个转圈的字符)
    pub title: String,
}

const FMT: &str = "#{@aca_id}\t#{@aca_agent}\t#{session_id}\t#{session_name}\t#{pane_id}\t#{pane_pid}\t#{pane_current_command}\t#{pane_dead}\t#{session_attached}\t#{session_last_attached}\t#{session_activity}\t#{window_index}\t#{pane_index}\t#{pane_current_path}\t#{window_activity}\t#{window_panes}\t#{window_name}\t#{pane_title}";

pub fn parse_panes(out: &str) -> Vec<Pane> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(18, '\t').collect();
            (f.len() == 18).then(|| Pane {
                id: f[0].into(),
                agent: f[1] == "1",
                sid: f[2].into(),
                session: f[3].into(),
                pane: f[4].into(),
                pid: f[5].parse().unwrap_or(0),
                cmd: f[6].into(),
                dead: f[7] == "1",
                attached: f[8].parse().unwrap_or(0),
                last_attached: f[9].parse().unwrap_or(0),
                activity: f[10].parse().unwrap_or(0),
                window: f[11].parse().unwrap_or(0),
                index: f[12].parse().unwrap_or(0),
                path: f[13].into(),
                win_activity: f[14].parse().unwrap_or(0),
                win_panes: f[15].parse().unwrap_or(1),
                win_name: f[16].into(),
                title: f[17].into(),
            })
        })
        .collect()
}

/// 全部窗格;tmux 服务没起来 = 空
pub fn panes() -> Vec<Pane> {
    run(&["list-panes", "-a", "-F", FMT]).map(|o| parse_panes(&o)).unwrap_or_default()
}

/// 一个会话里 agent 的那一格:记了 `@aca_agent` 的;没有(老会话、被人关了)就取编号最小的
pub fn agent_pane<'a>(panes: &'a [Pane], id: &str) -> Option<&'a Pane> {
    let mine = || panes.iter().filter(|p| p.id == id);
    mine().find(|p| p.agent).or_else(|| mine().min_by_key(|p| (p.window, p.index)))
}

pub fn find(id: &str) -> Option<Pane> {
    agent_pane(&panes(), id).cloned()
}

/// 这一格在哪个 aca 会话里(它那儿看得到的 `@aca_id`);不是 aca 的会话、问不到 = None
pub fn session_of(pane: &str) -> Option<String> {
    run(&["display-message", "-p", "-t", pane, "#{@aca_id}"]).ok().map(|o| o.trim().to_string()).filter(|s| !s.is_empty())
}

/// 标题 → tmux 会话名:`.` 和 `:` 在 tmux 的目标写法里有意思(`会话:窗口.窗格`),换掉;空白收成一个;太长截断
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

/// 重名的加 ` (2)`、` (3)` …
pub fn unique_name(want: &str, taken: &[String]) -> String {
    let mut name = want.to_string();
    let mut n = 2;
    while taken.contains(&name) {
        name = format!("{want} ({n})");
        n += 1;
    }
    name
}

/// 让会话名跟着标题走(agent 那个窗口的名字也是):Moshi 第一层列的是会话名,第二层是窗口名
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

/// 屏幕上现在的字(不带颜色):判断状态用
pub fn capture(pane: &str) -> Vec<String> {
    plain_lines(&run(&["capture-pane", "-p", "-t", pane]).unwrap_or_default())
}

/// `capture-pane -p` 打出来的 → 一行一行的字:行尾的空白、末尾的空行不要
pub fn plain_lines(out: &str) -> Vec<String> {
    let mut lines: Vec<String> = out.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// 好几格的屏幕一条 tmux 命令抓回来(`ansi` = 带颜色,`capture-pane -e`):顺序同 `panes`,一格一段,
/// 就是单独抓它时打出来的那些字;抓不到的那格是空的。
/// 一格起一个 tmux 的话,列表每刷新一次就是三十几个进程(2026-10-14 实测 37 格 143 毫秒 CPU,合成一条 3.6 毫秒)
pub fn capture_many(panes: &[&str], ansi: bool) -> Vec<String> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    // 记号每次现编:屏幕上碰巧有一样的一行(正开着这份源码)也分不错
    let mark = format!("aca-cut-{:x}-{:x}-{:x}", std::process::id(), nanos, N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    capture_with(panes, ansi, &mark, |args| {
        cmd().args(args).stderr(Stdio::null()).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    })
}

/// 每格抓完跟一条 `display-message -p` 打一行记号,回来靠它分段。
/// 记号里不能有 `#`、`%`(display-message 会当格式展开)和词尾的 `;`
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

/// 打出来的那一长串按记号行切开;最后没等到记号的半段不算
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

/// 有一格抓不到(刚关掉的)tmux 就停在那儿,后面的命令不跑了:那格记成空的,从下一格接着抓。
/// 每一轮至少往前一格,全都抓不到也就是一格一个进程(和原来一样)
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

/// 交给插件的字(`input = "screen" / "scrollback"`):折了行的接回一行(网址不断开),`back` = 连回滚一起(最多这么多行)
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

/// 带颜色的屏幕(`capture-pane -e`):输入框里哪些字是灰的(提示)、哪些是人打的,要看颜色
pub fn capture_ansi(pane: &str) -> Vec<String> {
    run(&["capture-pane", "-p", "-e", "-t", pane]).unwrap_or_default().lines().map(String::from).collect()
}

/// 你是不是正在这一格里打字:有终端正看着它,而且那个终端最后一次按键在 `secs` 秒以内。
/// tmux 记的「最后一次按键」只随终端里真的按键变,aca 自己 send-keys、粘贴都不改它(2026-10-08 实测)
pub fn typing(pane: &str, secs: u64) -> bool {
    let now = crate::paths::now();
    run(&["list-clients", "-F", "#{client_activity}\t#{pane_id}"]).unwrap_or_default().lines().any(|l| {
        let (at, p) = l.split_once('\t').unwrap_or_default();
        p == pane && now.saturating_sub(at.parse().unwrap_or(0)) < secs
    })
}

/// 往窗格按几个键(tmux 的键名:`2`、`Escape`、`Enter`)
pub fn keys(pane: &str, keys: &[&str]) -> Res<()> {
    let mut a = vec!["send-keys", "-t", pane];
    a.extend_from_slice(keys);
    run(&a).map(|_| ())
}

/// 把文字粘进窗格的输入框(括号粘贴,多行不会被当成发送),不按回车。
/// 窗格停在 copy-mode 里时先退出来:那时候粘进去的字是一个个敲给 tmux 的
pub fn paste(pane: &str, text: &str) -> Res<()> {
    let in_mode = || run(&["display-message", "-p", "-t", pane, "#{pane_in_mode}"]).is_ok_and(|o| o.trim() == "1");
    if in_mode() {
        let _ = run(&["copy-mode", "-q", "-t", pane]);
        if in_mode() {
            return Err(t!("窗格停在 tmux 模式里", "the pane is in a tmux mode").into());
        }
    }
    let buf = format!("aca-{}", std::process::id());
    run(&["set-buffer", "-b", &buf, "--", text])?;
    run(&["paste-buffer", "-p", "-d", "-b", &buf, "-t", pane]).map(|_| ())
}

const DETACH_KEY: &str = "C-q";
/// 在哪些会话里按它是回来:记了 `@aca_id` 的,和 agent-deck 起名的
const DETACH_IN: &str = "#{||:#{m:agentdeck_*,#{session_name}},#{?@aca_id,1,0}}";
/// `after-bind-key` 是个数组,aca 只占这一格:你自己挂的、agent-deck 挂的在别的格里,不碰
const DETACH_HOOK: &str = "after-bind-key[222]";
/// 钩子里:刚绑的是这个键,而且是 agent-deck 那一版(认它起的会话名、不认 `@aca_id`)。你自己绑别的不算
const DECK_BOUND: &str =
    "#{&&:#{==:#{hook_argument_0},C-q},#{&&:#{m:*agentdeck_*,#{hook_arguments}},#{==:#{m:*@aca_id*,#{hook_arguments}},0}}}";

/// 进去之后按 Ctrl+Q 回来。键绑在 tmux 服务上(不用前缀):aca 的会话里是 detach,别的会话里照样把键送给里面的程序。
/// agent-deck 也绑这个键,只认它自己起名的会话(`agentdeck_*`),别的会话里把键吞掉 —— 接管过来的会话改了名,它就不管了。
/// 所以它绑的换成两边都认的;你自己绑了别的不动。
/// 绑定不在 tmux.conf 里:服务重启、agent-deck 再起一个会话都会把它冲掉,所以建会话后、进会话前都来一遍。
/// 你人已经在会话里时 agent-deck 起会话(它每起一个就绑一次),这几处都轮不到:挂一个钩子,它一绑 tmux 当场换回来。
/// 进会话前要等它做完:一次 list-keys 看现状,要改的一次 tmux 调用做完
pub fn bind_detach() {
    let Ok(keys) = run(&["list-keys"]) else { return };
    let mut cmds = detach_changes(&keys);
    // 插件要的热键一样不在 tmux.conf 里:跟着一起补
    if let Some(want) = crate::plugin::hotkey_binds() {
        cmds.extend(hotkey_changes(&keys, &want).0);
    }
    let _ = run(&cmds.join(&";".to_string()));
}

/// Ctrl+Q 要跑的命令。钩子每次都挂(看一眼它在不在和挂一次一样贵),键上不是这一版的才绑。
/// 钩子在最前:一串命令里有一条出错,tmux 就不跑后面的了
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

/// 热键绑的命令里都带着它:「这是 aca 绑的」靠它认。你自己在 tmux.conf 里绑的 `aca plugin pane …` 没有它,不会被当成 aca 的解掉
pub const HOTKEY_MARK: &str = "--hotkey";
/// 热键只在 aca 的会话里拦
const HOTKEY_IN: &str = "#{?@aca_id,1,0}";
/// 在这几张键表里绑:平常(root),和往上翻着看的时候(copy-mode;vi 键位是 copy-mode-vi)
const HOTKEY_TABLES: [&str; 3] = ["root", "copy-mode", "copy-mode-vi"];

/// 插件的热键(清单的 `[[hotkeys]]`),和 Ctrl+Q 一个办法:绑在 tmux 服务上、不用前缀;aca 的会话里跑那条命令,
/// 别的会话里键照样送给里面的程序。`want`:(键, 按了跑的 shell 命令 —— 带着 [`HOTKEY_MARK`],里面的 `#{…}` tmux 会展开)。
/// aca 以前绑的、现在没人要的解掉;键上有别人绑的(你的 tmux.conf、tmux 自带的)不动。返回平常那张键表里没绑上的键。
/// tmux 没在跑:什么都不做
pub fn bind_hotkeys(want: &[(String, String)]) -> Vec<String> {
    let Ok(keys) = run(&["list-keys"]) else { return vec![] };
    let (cmds, held) = hotkey_changes(&keys, want);
    if !cmds.is_empty() {
        // 一次调用全做完:命令之间放一个单独的 `;`
        let _ = run(&cmds.join(&";".to_string()));
    }
    held
}

/// 现在的键表(`list-keys` 的输出)要变成 `want`,得跑哪些 tmux 命令;和平常那张键表里被别人占着的键
fn hotkey_changes(keys: &str, want: &[(String, String)]) -> (Vec<Vec<String>>, Vec<String>) {
    let s = |x: &str| x.to_string();
    let (mut cmds, mut held) = (vec![], vec![]);
    for table in HOTKEY_TABLES {
        let now: Vec<(&str, &str)> = bindings(keys, table).collect();
        for (k, _) in now.iter().filter(|(k, c)| c.contains(HOTKEY_MARK) && !want.iter().any(|(w, _)| w == k)) {
            cmds.push(vec![s("unbind-key"), s("-T"), s(table), s(k)]);
        }
        for (k, sh) in want {
            match now.iter().find(|(b, _)| b == k) {
                Some((_, c)) if !c.contains(HOTKEY_MARK) => {
                    if table == "root" {
                        held.push(k.clone());
                    }
                }
                Some((_, c)) if plain(c).contains(&plain(sh)) => {}
                _ => {
                    let go = format!("run-shell -b {}", quote(sh));
                    let mut c = vec![s("bind-key"), s("-T"), s(table), k.clone(), s("if-shell"), s("-F"), s(HOTKEY_IN), go];
                    // 平常:不是 aca 的会话,键原样送进去。翻着看的时候没有「送进去」这回事
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

/// `list-keys` 里一张键表的每一条:(键, 命令那一段)
fn bindings<'a>(keys: &'a str, table: &'a str) -> impl Iterator<Item = (&'a str, &'a str)> {
    keys.lines().filter_map(move |l| {
        let (t, rest) = l.split_once("-T")?.1.trim_start().split_once(char::is_whitespace)?;
        let (k, cmd) = rest.trim_start().split_once(char::is_whitespace)?;
        (t == table).then(|| (k, cmd.trim_start()))
    })
}

/// 一段字写成 tmux 命令里的一个参数(双引号里 `\` `"` `$` 要转义)
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

/// 去掉引号和转义:`list-keys` 打出来的命令引号加得和绑的时候不一样,比的时候不看它们
fn plain(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '\\' | '"' | '\'')).collect()
}

/// `list-keys` 里这个键在平常那张键表上绑的命令(`bind-key [-r] -T root C-q 命令 …` 的命令那一段)
fn bound<'a>(keys: &'a str, key: &str) -> Option<&'a str> {
    bindings(keys, "root").find(|(k, _)| *k == key).map(|(_, cmd)| cmd)
}

/// 没绑、agent-deck 绑的、aca 以前绑的(条件改过)→ 绑;已经是这一版、别人绑的 → 不动
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
        // 什么都没绑:三张键表各绑一条;平常那张不是 aca 的会话把键送进去
        let (cmds, held) = hotkey_changes("bind-key    -T root         M-.   next-window\n", &want);
        assert!(held.is_empty());
        assert_eq!(cmds.len(), 3);
        assert_eq!(cmds[0][..7], ["bind-key", "-T", "root", "C-]", "if-shell", "-F", HOTKEY_IN]);
        assert_eq!(cmds[0][7], format!("run-shell -b \"{}\"", sh("pick", "C-]")));
        assert_eq!((cmds[0][8].as_str(), cmds[1].len(), cmds[2][2].as_str()), ("send-keys C-]", 8, "copy-mode-vi"));
        // tmux 打出来的是这个样子(引号换了一种):已经是这一条,不用再绑
        let line = |table: &str, key: &str, pane: &str| {
            format!(
                "bind-key    -T {table} {key}  if-shell -F \"#{{?@aca_id,1,0}}\" \"run-shell -b \\\"'/bin/aca' plugin pane aca.copy {pane} --hotkey {key} --session '#{{@aca_id}}' --client '#{{client_name}}'\\\"\" \"send-keys {key}\"\n"
            )
        };
        let all = |key: &str, pane: &str| HOTKEY_TABLES.map(|t| line(t, key, pane)).concat();
        assert_eq!(hotkey_changes(&all("C-]", "pick"), &want), (vec![], vec![]));
        // 换了要开的窗格:重绑。换了键:旧的解掉、新的绑上。没人要了:全解掉
        assert_eq!(hotkey_changes(&all("C-]", "old"), &want).0.len(), 3);
        let (cmds, _) = hotkey_changes(&all("M-y", "pick"), &want);
        assert_eq!(cmds.iter().filter(|c| c[0] == "unbind-key" && c[3] == "M-y").count(), 3);
        assert_eq!(cmds.iter().filter(|c| c[0] == "bind-key" && c[3] == "C-]").count(), 3);
        let (cmds, _) = hotkey_changes(&all("C-]", "pick"), &[]);
        assert_eq!(cmds, HOTKEY_TABLES.map(|t| vec!["unbind-key".to_string(), "-T".into(), t.into(), "C-]".into()]));
        // 你自己绑的(哪怕也是开 aca 的窗格):不动,平常那张键表里的报出来
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
    fn ctrl_q_is_taken_from_agent_deck_but_not_from_you() {
        let line = |cmd: &str| {
            format!("bind-key    -T root         M-.   next-window\nbind-key -r -T root         C-q                       {cmd}\n")
        };
        assert_eq!(bound(&line("detach-client"), "C-q"), Some("detach-client"));
        assert_eq!(bound("bind-key  -T root M-. next-window\n", "C-q"), None);
        assert!(rebind(None));
        // agent-deck 绑的:只认它起名的会话
        let deck = line("if-shell -F \"#{m:agentdeck_*,#{session_name}}\" detach-client ''");
        assert!(rebind(bound(&deck, "C-q")));
        // 已经是这一版的:不用再绑
        let ours = line(&format!("if-shell -F \"{DETACH_IN}\" detach-client \"send-keys C-q\""));
        assert!(!rebind(bound(&ours, "C-q")));
        // 条件改过之前的那一版:换掉
        assert!(rebind(bound(&line("if-shell -F \"#{@aca_id}\" detach-client"), "C-q")));
        // 你自己绑的
        assert!(!rebind(bound(&line("display-popup -E htop"), "C-q")));
        // 读的是全部键表:别的表上的 C-q 不算
        assert_eq!(bound("bind-key -T copy-mode C-q send-keys -X cancel\n", "C-q"), None);
        // 钩子每次都挂、挂在最前;键已经是这一版的就不再绑
        let first = |cmds: &[Vec<String>]| cmds.iter().map(|c| c[0].clone()).collect::<Vec<_>>();
        assert_eq!(first(&detach_changes(&ours)), ["set-hook"]);
        assert_eq!(first(&detach_changes(&deck)), ["set-hook", "bind-key"]);
        assert_eq!(detach_changes(&deck)[1].last().map(String::as_str), Some("send-keys C-q"));
    }

    #[test]
    fn a_pane_title_may_contain_tabs_and_the_agent_pane_wins() {
        let out = "ab12\t\t$1\t标题\t%3\t100\tzsh\t0\t0\t\t5\t1\t0\t/p\t6\t2\tzsh\tshell\n\
                   ab12\t1\t$1\t标题\t%2\t99\tnode\t0\t1\t7\t5\t0\t0\t/p\t6\t2\tzsh\t✳ 带\t制表符\n\
                   \t\t$2\t别的\t%9\t1\tzsh\t1\t0\t\t5\t0\t0\t/\t\t1\tzsh\t\n";
        let v = parse_panes(out);
        assert_eq!(v.len(), 3);
        let a = agent_pane(&v, "ab12").unwrap();
        assert_eq!((a.pane.as_str(), a.title.as_str(), a.attached, a.last_attached), ("%2", "✳ 带\t制表符", 1, 7));
        assert!(v[2].dead && v[2].id.is_empty());
        // 没有记 @aca_agent 的:编号最小的那格
        let mut w = v.clone();
        w[1].agent = false;
        assert_eq!(agent_pane(&w, "ab12").unwrap().pane, "%2");
        assert!(agent_pane(&v, "nope").is_none());
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
        // 一段就是单独抓那格时打出来的字;空屏也是一段;最后没等到记号的半段不算;别的记号只是屏幕上的一行字
        assert_eq!(split_marked("a\n  b  \n\nCUT\n\nCUT\nCUT-old\nCUT\n半段\n", "CUT"), ["a\n  b  \n\n", "\n", "CUT-old\n"]);
        assert_eq!(plain_lines("a  \n\n b\n\n\n"), ["a", "", " b"]);

        // 假的 tmux:命令一条一条跑,碰到抓不到的那格就停,后面的不跑
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
        // 都在的话就一个进程;%2 没了:它前面的那次白跑了后半截,从 %3 接着再来一次
        assert_eq!(calls, [4, 2]);
        let mut calls = vec![];
        assert_eq!(capture_with(&["%1", "%3", "%4"], false, "CUT", |a| fake(&mut calls, a)).len(), 3);
        assert_eq!(calls, [3]);
        // 一格都抓不到(tmux 没了):一格一次,不会转不出来;没有格就不起 tmux
        let mut calls = vec![];
        assert_eq!(capture_with(&["%8", "%9"], false, "CUT", |a| fake(&mut calls, a)), ["", ""]);
        assert_eq!(calls, [2, 1]);
        assert!(capture_with(&[], false, "CUT", |_| panic!("起了 tmux")).is_empty());
    }
}
