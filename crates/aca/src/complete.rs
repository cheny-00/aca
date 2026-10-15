//! zsh 补全:`aca completion zsh` 打出一段脚本;装好后每按一次 Tab,它问 `aca __complete <打好的词…> <正在打的词>`。
//!
//! 补什么全照 help.rs 里的用法:`<id>` 补会话、`-g 分组` 补分组(多一个 `.` = 按目录定)、`[目录]` 交给 zsh 补目录、
//! 写死的词(`group rename`、`cost [1|7|30|all]`)原样补。所以加命令只要在 help.rs 写好用法,这里不用动;
//! 用法里出现新的占位词,在 `kind` 里说它补什么,或者记进 `FREE`(测试会查每个占位词都认得)。
//! 脚本本身很薄、不带命令表:加了命令、会话和分组变了,都不用重新生成。
//!
//! 输出一行一个候选:`种类\t值\t说明`,同一种排在一起;`dirs` 一行 = 让 zsh 自己补目录。
//! 装了 fzf-tab 的 zsh 里就是 fzf 挑(说明也搜得到:打标题、组名都行)。
//!
//! 照现在那种话的用法拆:中文的占位词是汉字、写死的词是 ASCII;英文的占位词全大写、写死的词小写。
//! `kind` 两种都认,测试查同一行用法两种话拆出来一样。

use aca_core::i18n::{Lang, lang};
use aca_core::runtime;
use aca_core::session::BY_DIR;
use aca_core::status;
use aca_core::store::Store;
use aca_core::{config, tmux, tr};

use crate::help::{self, GROUPS};

/// zsh 的补全函数。放进 fpath 叫 `_aca`(第一次 Tab 时载入),也能 `eval "$(aca completion zsh)"`。
/// 每一种候选的标题跟着生成时的那种话:fzf-tab 的预览靠它认是不是会话(`$group == *会话*`)
pub fn zsh() -> String {
    let head = match lang() {
        Lang::Zh => "command 命令 sub 子命令 flag 选项 session 会话 group 分组 tool 工具 tmux 'tmux 会话' topic 说明",
        Lang::En => "command command sub subcommand flag option session session group group tool tool tmux 'tmux session' topic topic",
    };
    ZSH.replace("{HEAD}", head)
}

const ZSH: &str = r#"#compdef aca
# aca 的 zsh 补全(aca completion zsh 生成)。候选每次都问 aca 自己(aca __complete),
# 加了命令、会话和分组变了都不用重新生成。装了 fzf-tab 就是用 fzf 挑。

_aca() {
  local -A head=({HEAD})
  local -a vals parts
  local line t tag= ret=1
  # 最后补一个空行:把最后那一种也交出去
  for line in "${(@f)$(${words[1]} __complete -- "${(@)words[2,CURRENT]}" 2>/dev/null)}" ''; do
    parts=("${(@ps:\t:)line}")
    t=${parts[1]}
    if [[ -n $tag && $t != $tag ]]; then
      if [[ $tag == dirs ]]; then
        _path_files -/ && ret=0
      else
        # -V:照 aca 给的先后(会话最近动过的在前),不按字母重排
        _describe -V -t $tag "${head[$tag]:-$tag}" vals && ret=0
      fi
      vals=()
    fi
    tag=$t
    [[ -z $t || $t == dirs ]] && continue
    vals+=("${parts[2]//:/\\:}${parts[3]:+:${parts[3]}}")
  done
  return ret
}

# fzf-tab 默认按字母重排:关掉,会话才是最近动过的在前(自己设过就不动)
[[ -n $(zstyle -L ':completion:*:aca:*' sort) ]] || zstyle ':completion:*:aca:*' sort false

if [[ $funcstack[1] == _aca ]]; then
  _aca "$@"
else
  compdef _aca aca
fi
"#;

/// 一个参数补什么
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// 登记的会话(`unarchive` 补归档着的)
    Session,
    /// 已有的分组;dot = 再给一个 `.`(按目录定组)
    Group {
        dot: bool,
    },
    Dir,
    /// `aca names` 里的:工具、账号
    Tool,
    /// 还没登记的 tmux 会话(`adopt` 用)
    Tmux,
    /// 随便写的(标题、一段话、数):不补
    Free,
}

/// 用法里的占位词补什么(中文的、英文的)
fn kind(p: &str) -> Kind {
    match p {
        "id" | "会话 id" => Kind::Session,
        "分组" | "GROUP" => Kind::Group { dot: true },
        // group 子命令里的「路径」是分组自己的路径(work/app)
        "路径" | "PATH" => Kind::Group { dot: false },
        "目录" | "默认目录" | "本地目录" | "DIR" | "DEFAULT-DIR" | "LOCAL-DIR" => Kind::Dir,
        "工具[:账号]" | "工具名" | "TOOL[:ACCOUNT]" | "TOOL-NAME" => Kind::Tool,
        "tmux 会话" | "%窗格" | "TMUX-SESSION" | "%PANE" => Kind::Tmux,
        _ => Kind::Free,
    }
}

/// 原样敲的词:小写字母、数字、- _(`popup`、`30`、`agent-deck`);英文的占位词全大写,不算
fn literal(w: &str) -> bool {
    w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// 认得、但随便写的占位词:不补
#[cfg(test)]
const FREE: &[&str] = &[
    "关键词",
    "客户端",
    "标题",
    "话",
    "给 agent 的参数…",
    "每次启动带的参数…",
    "对话id",
    "新路径",
    "位置",
    "秒",
    "时长",
    "名字",
    "值",
    "从哪套",
    "机器",
    "说明",
    "6h",
    "命令",
    "参数…",
    "命令…",
    "账号,…",
    "插件 id",
    "事件",
    "字段=值…",
    "插件.动作",
    "插件.视图",
    "插件.能力",
    "aca.名字",
    "名字…",
    "插件",
    "窗格",
    "任务 id",
    "本机端口",
    "地址",
    "行数",
    "''",
    "分支",
    "哪些",
    "话…",
    "条数",
    "选项号",
    "git 地址",
    "owner/repo",
    "版本",
    "名字=值…",
    "服务",
    "能力…",
    "目标",
    "键",
];

/// 用法里的一格:写死的词(几个里挑一个)和 / 或一个占位的参数
#[derive(Clone, Debug, PartialEq)]
struct Item {
    words: Vec<String>,
    kind: Option<Kind>,
    optional: bool,
    /// 能写好几个(`<id>…`)
    many: bool,
    /// 用法里的原文(选项的值写在说明里:`-g 分组`)
    text: String,
}

impl Item {
    fn takes(&self, w: &str) -> bool {
        self.kind.is_some() || self.words.iter().any(|x| x == w)
    }
}

/// 一种用法:位置参数一格一格,选项(带不带值),有没有 `--`(后面的原样交出去,不补)
#[derive(Clone, Debug, Default, PartialEq)]
struct Form {
    items: Vec<Item>,
    flags: Vec<(String, Option<Item>)>,
    rest: bool,
    /// 这行用法本身:子命令的说明
    usage: String,
}

/// `[ … ]` / `< … >` 里面的:`a | b`、`1|7|30|all`、`工具[:账号]`
fn choice(inner: &str, optional: bool, many: bool) -> Item {
    let mut it = Item { words: vec![], kind: None, optional, many, text: inner.trim().to_string() };
    for alt in inner.split('|').map(str::trim).filter(|a| !a.is_empty()) {
        match kind(alt) {
            Kind::Free if literal(alt) => it.words.push(alt.to_string()),
            Kind::Free if it.kind.is_none() => it.kind = Some(Kind::Free),
            Kind::Free => {}
            k => it.kind = Some(k),
        }
    }
    // 只有写死的词:就在这几个里挑
    it
}

/// 一行用法(不带开头的命令名)拆成一种 `Form`
fn parse(usage: &str) -> Form {
    let mut f = Form { usage: usage.to_string(), ..Form::default() };
    let cs: Vec<char> = usage.chars().collect();
    let mut i = 0;
    // 光秃秃的词之间的 `|`:几个里挑一个,并进上一格
    let mut alt = false;
    while i < cs.len() {
        match cs[i] {
            ' ' => i += 1,
            '[' | '<' => {
                let (open, close) = (cs[i], if cs[i] == '[' { ']' } else { '>' });
                let (mut depth, mut j) = (0, i);
                while j < cs.len() {
                    if cs[j] == open {
                        depth += 1;
                    } else if cs[j] == close {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    j += 1;
                }
                let inner: String = cs[i + 1..j.min(cs.len())].iter().collect();
                let many = cs.get(j + 1) == Some(&'…');
                i = j + 1 + usize::from(many);
                let inner = inner.trim();
                if inner == "--" || inner.starts_with("-- ") {
                    f.rest = true;
                } else if inner.starts_with('-') {
                    let (name, arg) = inner.split_once(' ').map_or((inner, None), |(n, a)| (n, Some(a.trim())));
                    let arg = arg.map(|a| match choice(a, false, false) {
                        // 只有一个词(`--within 6h`):是举的例子,随便写
                        it if it.words.len() == 1 && it.kind.is_none() => Item { words: vec![], kind: Some(Kind::Free), ..it },
                        it => it,
                    });
                    f.flags.push((name.to_string(), arg));
                } else {
                    f.items.push(choice(inner, open == '[', many));
                }
            }
            _ => {
                let j = (i..cs.len()).find(|&j| cs[j] == ' ').unwrap_or(cs.len());
                let w: String = cs[i..j].iter().collect();
                i = j;
                match w.as_str() {
                    "|" => alt = true,
                    // 后面的原样交给 agent / 远端
                    "--" => {
                        f.rest = true;
                        break;
                    }
                    w if w.starts_with('-') => f.flags.push((w.to_string(), None)),
                    w if alt => {
                        alt = false;
                        if let Some(last) = f.items.last_mut() {
                            last.words.push(w.to_string());
                        }
                    }
                    w => f.items.push(Item { words: vec![w.to_string()], kind: None, optional: false, many: false, text: w.to_string() }),
                }
            }
        }
    }
    f
}

/// 照一种用法走完打好的词:接下来要补什么。走不通(多了参数、不认得的选项)= None
enum Next<'a> {
    /// 刚打了一个要带值的选项
    Value(&'a Item),
    /// 从第几格位置参数接着
    At(usize),
    /// `--` 之后:不补
    Rest,
}

fn walk<'a>(f: &'a Form, typed: &[String]) -> Option<Next<'a>> {
    let mut at = 0;
    let mut pending: Option<&Item> = None;
    for w in typed {
        if pending.take().is_some() {
            continue;
        }
        if w == "--" {
            return f.rest.then_some(Next::Rest);
        }
        if w.starts_with('-') && w.len() > 1 {
            pending = f.flags.iter().find(|x| x.0 == *w)?.1.as_ref();
            continue;
        }
        loop {
            let it = f.items.get(at)?;
            if it.takes(w) {
                at += usize::from(!it.many);
                break;
            }
            if !it.optional {
                return None;
            }
            at += 1;
        }
    }
    Some(pending.map_or(Next::At(at), Next::Value))
}

/// 一个候选:(种类, 值, 说明)
type Cand = (&'static str, String, String);

/// 细节页里给这个词的那条说明:开头两个空格,词(带值的选项连着值)后面就是说明,
/// 缩进更深的下一行是接着的:
///
/// ```text
///   -g 分组        可以带层级(work/app),没有就建。
///                  不写:在某个会话里建的跟它进同一组
/// ```
fn note(more: &str, word: &str, arg: &str) -> Option<String> {
    let lines: Vec<&str> = more.lines().collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    for (i, l) in lines.iter().enumerate() {
        let Some(rest) = l.trim_start().strip_prefix(word).filter(|_| indent(l) == 2) else { continue };
        let rest = if arg.is_empty() { rest } else { rest.trim_start().strip_prefix(arg).unwrap_or(rest) };
        if !rest.starts_with(' ') || rest.trim().is_empty() {
            continue;
        }
        let mut text = rest.trim().to_string();
        // 接着的行:英文在两行之间补个空格,中文不补
        let wide = |c: char| c >= '\u{2E80}';
        for next in lines[i + 1..].iter().take_while(|n| !n.trim().is_empty() && indent(n) > indent(l) + 2) {
            if !text.ends_with(wide) && !next.trim().starts_with(wide) {
                text.push(' ');
            }
            text.push_str(next.trim());
        }
        return Some(text);
    }
    None
}

/// 一个命令名的那一句。几个名字共用一项、一句话按「/」分成几段的(start / stop):各取各的那段
fn brief(c: &help::Cmd, name: &str) -> String {
    let parts: Vec<&str> = c.brief.split('/').map(str::trim).collect();
    match c.names.iter().position(|n| *n == name) {
        Some(i) if c.names.len() > 1 && parts.len() == c.names.len() => parts[i].to_string(),
        _ => c.brief.to_string(),
    }
}

/// 写死的词的说明:子命令带上它后面的用法(`rename` → `<路径> <新路径> · 改名…`),再加细节页里的那条
fn word_note(c: &help::Cmd, usage: &str, w: &str) -> String {
    // 开头几个子命令共用后面的用法:`unlink | enable | log <插件 id>`
    let mut toks = usage.split(' ').peekable();
    let mut lead: Vec<&str> = toks.next().into_iter().collect();
    while toks.next_if_eq(&"|").is_some() {
        lead.extend(toks.next());
    }
    let rest = if lead.contains(&w) { toks.collect::<Vec<_>>().join(" ") } else { String::new() };
    [rest, note(&c.more, w, "").unwrap_or_default()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
}

/// 选项的说明:`分组 · 可以带层级…`
fn flag_note(c: &help::Cmd, name: &str, arg: Option<&Item>) -> String {
    let text = arg.map_or("", |a| a.text.as_str());
    [text.to_string(), note(&c.more, name, text).unwrap_or_default()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
}

/// 打好的词(`aca` 后面的,最后一个是正在打的)→ 候选
pub fn complete(words: &[String]) -> Vec<Cand> {
    complete_with(words, &values)
}

/// values:一种参数的候选从哪来(测试里换成假的,不碰登记表)
fn complete_with(words: &[String], values: &dyn Fn(Kind, &str) -> Vec<Cand>) -> Vec<Cand> {
    let Some((cur, typed)) = words.split_last() else { return vec![] };
    let mut out: Vec<Cand> = vec![];
    let Some((cmd, args)) = typed.split_first() else {
        if !cur.starts_with('-') {
            for c in GROUPS.iter().flat_map(|g| g.1.iter()) {
                out.extend(c.names.iter().map(|n| ("command", n.to_string(), brief(c, n))));
            }
            out.push(("command", "help".into(), tr!("看说明:aca help <命令>", "read the help: aca help <COMMAND>")));
            out.push(("command", "version".into(), tr!("这个 aca 是哪天装的", "which build of aca this is")));
        }
        return out;
    };
    if cmd == "help" {
        if args.is_empty() {
            for c in GROUPS.iter().flat_map(|g| g.1.iter()) {
                out.extend(c.names.iter().map(|n| ("command", n.to_string(), brief(c, n))));
            }
            out.extend(help::TOPICS.iter().map(|(n, what)| ("topic", n.to_string(), what.to_string())));
        }
        return out;
    }
    let Some(c) = help::find(cmd) else { return out };
    // 几个名字共用一项的(start / stop):只看这个名字的用法
    let forms: Vec<Form> =
        c.usage.iter().filter_map(|u| u.split_once(' ').map_or(Some(""), |(n, rest)| (n == cmd).then_some(rest))).map(parse).collect();
    let mut flags: Vec<(String, String)> = vec![];
    let mut seen_flag = |name: &str, arg: Option<&Item>| {
        if !typed.iter().any(|w| w == name) && !flags.iter().any(|f| f.0 == name) {
            flags.push((name.to_string(), flag_note(c, name, arg)));
        }
    };
    let mut items: Vec<(&Item, &str)> = vec![];
    for f in &forms {
        match walk(f, args) {
            None | Some(Next::Rest) => {}
            Some(Next::Value(it)) => items.push((it, "")),
            Some(Next::At(at)) => {
                for (name, arg) in &f.flags {
                    seen_flag(name, arg.as_ref());
                }
                // 可选的那几格连着往后看,到第一个非写不可的为止
                for it in &f.items[at.min(f.items.len())..] {
                    items.push((it, &f.usage));
                    if !it.optional {
                        break;
                    }
                }
            }
        }
    }
    let wants_flag = cur.starts_with('-');
    if !wants_flag {
        for (it, usage) in &items {
            out.extend(it.words.iter().map(|w| ("sub", w.clone(), word_note(c, usage, w))));
            if it.kind == Some(Kind::Group { dot: true }) {
                out.push((
                    "group",
                    BY_DIR.to_string(),
                    tr!("按目录定的组(没有就以目录名新建)", "the group its directory decides (named after it if new)"),
                ));
            }
            if let Some(k) = it.kind {
                out.extend(values(k, cmd));
            }
        }
    }
    // 打了 - 才列选项;没别的可补时也列(fzf 里一眼看到还能加什么)
    if wants_flag || out.is_empty() {
        out.extend(flags.into_iter().map(|(n, what)| ("flag", n, what)));
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|(t, v, _)| seen.insert((*t, v.clone())));
    // 同一种排在一起(脚本按种类分组交给 zsh)
    let order = |t: &str| ["sub", "session", "group", "tool", "tmux", "dirs", "flag"].iter().position(|x| *x == t).unwrap_or(9);
    out.sort_by_key(|c| order(c.0));
    out
}

/// 一种参数的候选
fn values(k: Kind, cmd: &str) -> Vec<Cand> {
    match k {
        Kind::Free => vec![],
        Kind::Dir => vec![("dirs", String::new(), String::new())],
        Kind::Session => sessions(cmd == "unarchive"),
        Kind::Group { .. } => {
            let mut v = vec![];
            if let Ok(st) = Store::open() {
                let mut n = std::collections::HashMap::<String, usize>::new();
                for s in st.sessions(false).unwrap_or_default() {
                    *n.entry(s.group).or_default() += 1;
                }
                for g in st.groups().unwrap_or_default() {
                    let k = n.get(&g.path).copied().unwrap_or(0);
                    let s = if k == 1 { "" } else { "s" };
                    let mut what = vec![tr!("{k} 个会话", "{k} session{s}")];
                    if !g.default_path.is_empty() {
                        what.push(tilde(&g.default_path));
                    }
                    v.push(("group", g.path, what.join(" · ")));
                }
            }
            v
        }
        Kind::Tool => {
            let cfg = config::load();
            cfg.names().into_iter().map(|n| ("tool", n.clone(), tool_note(&cfg, &n))).collect()
        }
        Kind::Tmux => {
            let f = "#{session_name}\t#{@aca_id}\t#{pane_current_command}\t#{pane_current_path}";
            let out = tmux::run(&["list-sessions", "-F", f]).unwrap_or_default();
            out.lines()
                .filter_map(|l| {
                    let f: Vec<&str> = l.split('\t').collect();
                    (f.len() == 4 && f[1].is_empty()).then(|| ("tmux", f[0].to_string(), format!("{} · {}", f[2], tilde(f[3]))))
                })
                .collect()
        }
    }
}

/// 工具名的说明:账号带的那几样(配置目录、参数…),工具不写账号时用哪个
fn tool_note(cfg: &config::Config, name: &str) -> String {
    if let Some((kind, acct)) = name.split_once(':') {
        let Some(a) = cfg.account(kind, acct) else { return String::new() };
        let mut v = vec![];
        if !a.home.is_empty() {
            v.push(tilde(&a.home));
        }
        if !a.args.is_empty() {
            v.push(a.args.join(" "));
        }
        if !a.env.is_empty() {
            v.push(tr!("环境 {}", "env {}", a.env.keys().cloned().collect::<Vec<_>>().join(" ")));
        }
        if !a.env_file.is_empty() {
            v.push(tr!("环境读 {}", "env from {}", tilde(&a.env_file)));
        }
        return v.join(" · ");
    }
    let Some(t) = cfg.tool(name) else { return String::new() };
    let what = match t.kind.as_str() {
        "shell" => return tr!("普通 shell,不起 agent", "a plain shell, no agent"),
        "" => tr!("声明式工具", "declared tool"),
        k if k == name => String::new(),
        k => tr!("按 {k} 起", "starts as {k}"),
    };
    let acct = if t.account.is_empty() {
        tr!("不写账号:用它自己的配置目录", "no account given: its own config directory")
    } else {
        tr!("不写账号 = {}", "default account = {}", t.account)
    };
    [what, acct].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
}

/// 登记的会话,最近动过的在前;说明是「标题 · 分组」,fzf 里打标题、组名都找得到
fn sessions(archived: bool) -> Vec<Cand> {
    let Ok(st) = Store::open() else { return vec![] };
    let mut rows: Vec<(u64, Cand)> = st
        .sessions(true)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| (s.archived > 0) == archived)
        .map(|s| {
            let r = runtime::load(&s.id);
            let group = if s.group.is_empty() { tr!("没分组", "no group") } else { s.group.clone() };
            (r.ts.max(s.created), ("session", s.id.clone(), format!("{} · {group}", status::title(&s, &r))))
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    rows.into_iter().map(|r| r.1).collect()
}

fn tilde(p: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match p.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => p.to_string(),
    }
}

/// `aca __complete -- <词…>`:一行一个候选
pub fn print(words: &[String]) {
    for (t, v, d) in complete(words) {
        // 一行一个:值、说明里不能有换行 / tab
        let clean = |s: &str| s.replace(['\t', '\n'], " ");
        if t == "dirs" {
            println!("dirs");
        } else {
            println!("{t}\t{}\t{}", clean(&v), clean(&d));
        }
    }
}

#[cfg(test)]
mod tests {
    use aca_core::i18n::with;

    use super::*;

    fn w(s: &str) -> Vec<String> {
        // 最后一个空格后面是正在打的词(可以是空的)
        s.split(' ').map(String::from).collect()
    }

    /// 假的候选:会话、分组…各给一个占位的,目录照真的(交给 zsh)
    fn fake(k: Kind, _cmd: &str) -> Vec<Cand> {
        match k {
            Kind::Free => vec![],
            Kind::Dir => vec![("dirs", String::new(), String::new())],
            Kind::Session => vec![("session", "<会话>".into(), String::new())],
            Kind::Group { .. } => vec![("group", "<分组>".into(), String::new())],
            Kind::Tool => vec![("tool", "<工具>".into(), String::new())],
            Kind::Tmux => vec![("tmux", "<tmux>".into(), String::new())],
        }
    }

    fn cands(s: &str) -> Vec<Cand> {
        complete_with(&w(s), &fake)
    }

    fn got(s: &str) -> Vec<(String, String)> {
        cands(s).into_iter().map(|c| (c.0.to_string(), c.1)).collect()
    }

    fn has(s: &str, kind: &str, val: &str) -> bool {
        got(s).iter().any(|(k, v)| k == kind && v == val)
    }

    #[test]
    fn every_placeholder_in_the_usage_table_is_known() {
        let mut odd = vec![];
        for c in GROUPS.iter().flat_map(|g| g.1.iter()) {
            for u in c.usage {
                for p in placeholders(u) {
                    if kind(&p) == Kind::Free && !FREE.contains(&p.as_str()) && !p.chars().all(|c| c.is_ascii_alphanumeric()) {
                        odd.push(format!("{u}:{p}"));
                    }
                }
            }
        }
        assert!(odd.is_empty(), "这些占位词不知道补什么(写进 kind 或 FREE):{odd:#?}");
    }

    /// 补全里每个写死的词、每个选项都有一句说明:细节页里给它一条(`  --hold   只登记…`),两种话都要
    #[test]
    fn every_subcommand_and_flag_has_a_line_in_its_help_page() {
        let mut missing = vec![];
        for l in [Lang::Zh, Lang::En] {
            with(l, || {
                for c in GROUPS.iter().flat_map(|g| g.1.iter()) {
                    for u in c.usage {
                        let (name, rest) = u.split_once(' ').unwrap_or((u, ""));
                        let f = parse(rest);
                        for (flag, arg) in &f.flags {
                            if note(&c.more, flag, arg.as_ref().map_or("", |a| a.text.as_str())).is_none() {
                                missing.push(format!("{l:?} {name} {flag}"));
                            }
                        }
                        let args = f.flags.iter().filter_map(|x| x.1.as_ref());
                        for it in f.items.iter().chain(args) {
                            for w in &it.words {
                                if note(&c.more, w, "").is_none() && !missing.contains(&format!("{l:?} {name} {w}")) {
                                    missing.push(format!("{l:?} {name} {w}"));
                                }
                            }
                        }
                    }
                }
            });
        }
        assert!(missing.is_empty(), "这些在 help.rs 的细节页里没有说明:{missing:#?}");
    }

    /// 同一行用法,中文、英文拆出来一样:补什么、哪几个选项、哪些写死的词、哪格可以不写。只有原文不一样
    #[test]
    fn both_languages_of_a_usage_parse_the_same() {
        let shape = |f: Form| {
            let item = |i: &Item| (i.words.clone(), i.kind, i.optional, i.many);
            (
                f.items.iter().map(item).collect::<Vec<_>>(),
                f.flags.iter().map(|(n, a)| (n.clone(), a.as_ref().map(item))).collect::<Vec<_>>(),
                f.rest,
            )
        };
        for c in GROUPS.iter().flat_map(|g| g.1.iter()) {
            for u in c.usage.iter().chain([&c.short]) {
                assert_eq!(shape(parse(u.zh)), shape(parse(u.en)), "\n{}\n{}", u.zh, u.en);
            }
        }
        // 英文的占位词大写:不是要原样敲的词
        assert_eq!(parse("<TITLE> [-t TITLE]").items[0].kind, Some(Kind::Free));
        assert_eq!(parse("rename <id> <TITLE>").items[0].words, ["rename"]);
        // 接着的行:英文中间补空格,中文不补
        assert_eq!(
            note("  -g GROUP    can be nested,\n              created if missing", "-g", "GROUP").as_deref(),
            Some("can be nested, created if missing")
        );
    }

    #[test]
    fn notes_come_from_the_help_page_and_a_shared_brief_splits_per_name() {
        let more = "  -t 标题        不写 = 目录名\n  -g 分组        可以带层级,\n                 不写:没分组\n  --hold         只登记\n\n  --as 工具[:账号] 照这个登记";
        assert_eq!(note(more, "-t", "标题").as_deref(), Some("不写 = 目录名"));
        assert_eq!(note(more, "-g", "分组").as_deref(), Some("可以带层级,不写:没分组"));
        assert_eq!(note(more, "--hold", "").as_deref(), Some("只登记"));
        assert_eq!(note(more, "--as", "工具[:账号]").as_deref(), Some("照这个登记"));
        assert_eq!(note(more, "--ho", ""), None);
        // 表外的行(没缩进的、缩进更深的)不算:字符串里 `\` 换行吃掉了开头的空格就查得出来
        assert_eq!((note("--x  丢了缩进", "--x", ""), note("    --x  接着上一条的", "--x", "")), (None, None));
        let start = help::find("start").unwrap();
        assert_eq!(
            (brief(start, "start"), brief(start, "stop")),
            ("启动(接着原来的对话)".into(), "停下(agent 退出,窗格和登记留着)".into())
        );
        let d = |s: &str, v: &str| cands(s).into_iter().find(|c| c.1 == v).map(|c| c.2).unwrap_or_default();
        assert!(d("group ", "rename").starts_with("<路径> <新路径> · 改名"), "{}", d("group ", "rename"));
        assert!(d("new claude -", "-g").starts_with("分组 · 可以带层级"), "{}", d("new claude -", "-g"));
        assert!(d("new claude -", "--hold").starts_with("只登记"), "{}", d("new claude -", "--hold"));
    }

    #[test]
    fn tools_say_what_their_account_brings() {
        let cfg = config::parse(
            "[accounts.codex]\nlabs = { home = \"/x/.labs\", args = [\"--profile\", \"labs\"] }\n[tools.claude]\naccount = \"sub\"\n\
             [tools.ccx]\nkind = \"claude\"\n",
        );
        assert_eq!(tool_note(&cfg, "codex:labs"), "/x/.labs · --profile labs");
        assert_eq!(tool_note(&cfg, "claude"), "不写账号 = sub");
        assert_eq!(tool_note(&cfg, "codex"), "不写账号:用它自己的配置目录");
        assert_eq!(tool_note(&cfg, "ccx"), "按 claude 起 · 不写账号:用它自己的配置目录");
        assert_eq!(tool_note(&cfg, "shell"), "普通 shell,不起 agent");
    }

    /// 用法里括号中的占位词(选项名去掉,a | b 拆开)
    fn placeholders(u: &str) -> Vec<String> {
        let mut out = vec![];
        let mut depth = 0;
        let mut cur = String::new();
        for ch in u.chars() {
            match ch {
                '[' | '<' if depth == 0 => {
                    depth = 1;
                    cur.clear();
                }
                '[' | '<' => {
                    depth += 1;
                    cur.push(ch);
                }
                ']' | '>' if depth == 1 => {
                    depth = 0;
                    let inner = cur.trim();
                    let inner = if inner.starts_with('-') { inner.split_once(' ').map_or("", |x| x.1) } else { inner };
                    out.extend(inner.split('|').map(|a| a.trim().to_string()).filter(|a| !a.is_empty() && a != "--"));
                }
                ']' | '>' => {
                    depth -= 1;
                    cur.push(ch);
                }
                _ if depth > 0 => cur.push(ch),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn usages_parse_into_positions_flags_and_rest() {
        let f = parse("<工具[:账号]> [目录] [-t 标题] [-g 分组] [--hold] [-- 给 agent 的参数…]");
        assert_eq!(f.items.iter().map(|i| (i.kind, i.optional)).collect::<Vec<_>>(), [(Some(Kind::Tool), false), (Some(Kind::Dir), true)]);
        assert_eq!(f.flags.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["-t", "-g", "--hold"]);
        assert_eq!(f.flags[1].1.as_ref().unwrap().kind, Some(Kind::Group { dot: true }));
        assert!(f.rest);
        let f = parse("unlink | enable | disable | log <插件 id>");
        assert_eq!(f.items[0].words, ["unlink", "enable", "disable", "log"]);
        assert_eq!(f.items[1].kind, Some(Kind::Free));
        let f = parse("[1|7|30|all] [--only 账号,…]");
        assert_eq!((f.items[0].words.len(), f.items[0].kind, f.items[0].optional), (4, None, true));
        let f = parse("<tmux 会话 | %窗格>… [--as 工具[:账号]] [--placement popup|split|window]");
        assert_eq!((f.items[0].kind, f.items[0].many), (Some(Kind::Tmux), true));
        assert_eq!(f.flags[0].1.as_ref().unwrap().kind, Some(Kind::Tool));
        assert_eq!(f.flags[1].1.as_ref().unwrap().words, ["popup", "split", "window"]);
        let f = parse("<claude | codex | 工具名>");
        assert_eq!((f.items[0].words.clone(), f.items[0].kind), (vec!["claude".to_string(), "codex".to_string()], Some(Kind::Tool)));
        let f = parse("run <机器> [-m 说明] -- 命令");
        assert_eq!((f.items.len(), f.rest), (2, true));
    }

    #[test]
    fn the_first_word_is_a_command_and_help_lists_commands_and_topics() {
        assert!(has("", "command", "move") && has("", "command", "group") && has("", "command", "help"));
        assert!(has("mo", "command", "move")); // 前缀交给 zsh 过滤
        assert!(has("help ", "command", "new") && has("help ", "topic", "keys"));
        assert!(got("nosuch ").is_empty());
    }

    #[test]
    fn subcommands_then_their_own_arguments() {
        for sub in ["ls", "add", "create", "rename", "dir", "rm", "order"] {
            assert!(has("group ", "sub", sub), "{sub}");
        }
        // group add [目录]:补目录;group dir <路径> [目录 | '']:先分组(不带 .),再目录
        assert_eq!(got("group add "), [("dirs".to_string(), String::new())]);
        assert_eq!(got("group dir "), [("group".to_string(), "<分组>".to_string())]);
        assert!(has("group dir work ", "dirs", ""));
        assert!(!has("group dir work ", "sub", "''"));
        assert!(has("cost ", "sub", "all") && has("cost ", "sub", "7"));
        assert!(has("plugin timer ", "sub", "on") && has("plugin ", "sub", "enable"));
        assert!(has("theme ", "sub", "use") && has("theme ", "sub", "new"));
        // 写死的词对不上:那种用法不算
        assert!(got("group nosuch ").is_empty());
    }

    #[test]
    fn flags_show_after_a_dash_or_when_nothing_else_is_left_and_their_values_complete() {
        let fl = |s: &str| got(s).into_iter().filter(|c| c.0 == "flag").map(|c| c.1).collect::<Vec<_>>();
        assert!(fl("new claude -").contains(&"-g".to_string()));
        // 打过的不再列
        assert!(!fl("new claude -g work -").contains(&"-g".to_string()));
        // 还有位置参数要补(目录)时不列选项;打了 - 才列
        assert!(fl("new claude ").is_empty() && has("new claude ", "dirs", ""));
        assert!(fl("new claude ~/x ").contains(&"--hold".to_string()));
        // 选项的值:-g 后面补分组(连 .),--placement 补那几个词
        assert_eq!(got("new claude -g "), [("group".to_string(), ".".to_string()), ("group".to_string(), "<分组>".to_string())]);
        assert_eq!(got("new "), [("tool".to_string(), "<工具>".to_string())]);
        assert!(has("plugin pane x y --placement ", "sub", "split"));
        // -- 之后原样交给 agent:不补
        assert!(cands("new claude ~/x -- ").is_empty());
        // 不认得的选项:那种用法走不通
        assert!(fl("ls --nope -").is_empty());
    }

    #[test]
    fn rename_offers_auto_and_move_offers_dot() {
        assert_eq!(got("move ").first(), Some(&("session".to_string(), "<会话>".to_string())));
        assert!(has("move abc ", "group", "."));
        assert!(has("rename abc -", "flag", "--auto"));
        // 位置参数写满了:没有可补的
        assert!(got("move abc work ").is_empty());
        // adopt 补没登记的 tmux 会话,能写好几个
        assert!(has("adopt ", "tmux", "<tmux>") && has("adopt a b ", "tmux", "<tmux>"));
        // unarchive 也是会话(真的那份只列归档着的)
        assert!(has("unarchive ", "session", "<会话>"));
    }
}
