//! 界面的颜色,像 kitty 的主题:一套一套有名字,内置 latte(浅色,默认)和 mocha(深色),自己的放
//! ~/.config/aca/themes/<名字>.toml。用 `aca theme use <名字>` 换(或者 `^S` 设置页的「主题」:↑↓ 整屏实时预览,
//! ui.rs 的 `open_themes`);用哪套记在 config.toml:
//!
//! ```toml
//! [theme]
//! name = "latte"
//! ```
//!
//! 环境变量 ACA_THEME=<名字> 压过它(dark / light 是 mocha / latte 的别名。手机经 mosh 连过来时查不到终端底色,没法自动判断深浅)。
//!
//! 主题文件只写跟 base 不一样的,颜色的名字就是 `aca theme` 列的那些(`aca theme new <名字>` 生成一份全注释掉的):
//!
//! ```toml
//! base = "latte"      # 从哪套改起,不写 = latte。和内置同名的文件盖掉内置那套,它里面的 base = "latte" 指内置的
//! dark = false        # 是不是深色主题(列表里分开列),不写跟 base
//! accent = "#1E66F5"
//! series = ["#5992E7", "#F09362", "#51BFBB", "#C66CA1", "#71B262", "#DF8E1D"]   # 多写一个 = 多一个账号有颜色
//! ```
//!
//! 颜色写 "#RRGGBB"、终端的颜色名(red、lightblue、darkgray…,跟着终端的配色走)、0~255 的色号,或 "reset"(终端默认的字色)。
//! 存盘后开着的 aca 一秒内换上;名字、颜色写错的当没写(series / parts 里有一个错就整组当没写),`aca theme` 会列出来。
//! 底色和正文的字不归这里管,用终端自己的。
//!
//! 内置的两套:终端的 16 色里没有「淡紫底」这种颜色,只能写死 RGB;状态、次要的字、提示、账号也写死:
//! 终端自己的黄、绿在浅底上对比度只有 2~3,DIM(kitty 默认按 0.4 变淡)只有 1.9,次要的字整片都要费劲认。
//! latte 的字对比度都 ≥ 4.3(分隔符除外)。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

use aca_core::i18n::Tr;
use aca_core::{t, tr};

/// 每一种是什么、用在哪见 `tokens()`(主题文件里写的名字就是字段名)
#[derive(Clone, Copy)]
pub struct Pal {
    pub accent: Color,
    pub on_accent: Color,
    pub sel: Color,
    pub fade: Color,
    pub shadow: Color,
    pub minor: Color,
    pub faint: Color,
    pub warn: Color,
    pub hint: Color,
    pub hint_typed: Color,
    pub on_hint: Color,
    pub red: Color,
    pub green: Color,
    pub amber: Color,
    pub muted: Color,
    pub hunk: Color,
    pub claude: Color,
    pub codex: Color,
    pub total: Color,
    pub other: Color,
    pub track: Color,
    /// 计费面板里账号的颜色,一个账号一个位子(位子记在 slots.rs,跟着账号走、不跟排名走),有几个颜色就有几个位子。
    /// 默认是 Catppuccin 的 blue / peach / teal / pink / green 这几个色相,避开状态的红、黄和强调的紫(紫的归不分账号的条)。
    /// 原色是写字用的,铺成条太艳;彩度压得太低、亮度又在中间,暖色就发脏(砖红、灰紫)。所以彩度留在 0.13 上下
    /// (teal 到不了,0.10),暖色往亮里放:浅色主题亮度 0.65~0.75,深色主题 0.53~0.65。
    /// 顺序和每个的亮度是跑配色验证挑的(浅色底 #EFF1F5、深色底 #1E1E2E):相邻两个在色盲模拟和正常视觉下都分得清
    /// (粉和青色相撞,靠亮度差开,所以粉最深),前三个两两都分得清;
    /// 再加第六个(lavender / sky / flamingo …)总会和前面某个撞色,所以默认只有五个,多的账号画 other 的灰
    pub series: &'static [Color],
    /// 钱花在哪那根条的五段:强调色那个色相从浅到深(深色主题从深到浅),不跟账号、状态抢颜色;
    /// 最浅的那段在底色上对比度也有 2:1
    pub parts: [Color; 5],
    /// 星期 × 钟点的四档,花得越多越深
    pub heat: [Color; 4],
    /// 代码上色(见 `hl`):关键字、字符串、数字和常量、函数、类型、注释
    pub syn_kw: Color,
    pub syn_str: Color,
    pub syn_num: Color,
    pub syn_fn: Color,
    pub syn_ty: Color,
    pub syn_com: Color,
    /// 改动里加的 / 删的那行的底色
    pub add_bg: Color,
    pub del_bg: Color,
}

/// (分类, 名字, 用在哪, 那个字段)。`aca theme` 按这个顺序列,改颜色按名字找
fn tokens(p: &mut Pal) -> [(&'static str, &'static str, &'static str, &mut Color); 29] {
    [
        (
            t!("界面", "Interface"),
            "accent",
            t!(
                "强调色:顶上的标签、预览的框、底栏的主操作、弹窗的边框",
                "accent: the tabs on top, the preview frame, the main action in the bottom bar, popup borders"
            ),
            &mut p.accent,
        ),
        (t!("界面", "Interface"), "on_accent", t!("强调色底上的字", "text on the accent color"), &mut p.on_accent),
        (t!("界面", "Interface"), "sel", t!("选中项整块的底色", "background of the selected item"), &mut p.sel),
        (t!("界面", "Interface"), "fade", t!("弹窗打开时底下那层的字", "text of the layer under an open popup"), &mut p.fade),
        (t!("界面", "Interface"), "shadow", t!("弹窗的阴影", "popup shadow"), &mut p.shadow),
        (
            t!("界面", "Interface"),
            "minor",
            t!("次要的字:编号、时间、目录、计数、按键的说明", "minor text: numbers, times, directories, counts, key hints"),
            &mut p.minor,
        ),
        (
            t!("界面", "Interface"),
            "faint",
            t!("只是分隔用的符号:• │ ▾ └,小标题后面的线", "separator symbols: • │ ▾ └, the line after a subheading"),
            &mut p.faint,
        ),
        (
            t!("界面", "Interface"),
            "warn",
            t!("提示 / 警告的字,便签和置顶的记号", "hint / warning text, the marks for notes and pins"),
            &mut p.warn,
        ),
        (
            t!("界面", "Interface"),
            "hint",
            t!("空格跳转时还要按的字母的底色", "background of the letters still to type when jumping with space"),
            &mut p.hint,
        ),
        (
            t!("界面", "Interface"),
            "hint_typed",
            t!("空格跳转时已经按了的字母的底色", "background of the letters already typed when jumping"),
            &mut p.hint_typed,
        ),
        (t!("界面", "Interface"), "on_hint", t!("跳转字母本身", "the jump letters themselves"), &mut p.on_hint),
        (
            t!("状态", "State"),
            "red",
            t!(
                "◐ 待确认 / ✕ 出错;删除的确认框、改动页删掉的行、额度 ≥ 90%",
                "◐ needs you / ✕ error; delete confirmations, removed lines in diffs, quota ≥ 90%"
            ),
            &mut p.red,
        ),
        (t!("状态", "State"), "green", t!("● 在跑;改动页加的行", "● running; added lines in diffs"), &mut p.green),
        (t!("状态", "State"), "amber", t!("◐ 跑完了没看;额度 ≥ 70%", "◐ finished, unread; quota ≥ 70%"), &mut p.amber),
        (t!("状态", "State"), "muted", t!("○ 空闲 / ■ 没在跑", "○ idle / ■ not running"), &mut p.muted),
        (t!("状态", "State"), "hunk", t!("改动页里 @@ 那一行、改了名的文件", "the @@ line in diffs, renamed files"), &mut p.hunk),
        (t!("账号", "Accounts"), "claude", t!("列表里 Claude 的账号名", "Claude account names in the list"), &mut p.claude),
        (t!("账号", "Accounts"), "codex", t!("列表里 Codex 的账号名", "Codex account names in the list"), &mut p.codex),
        (
            t!("计费", "Billing"),
            "total",
            t!(
                "不分账号的条:走势小图、按模型、按目录、额度没到 70% 的条",
                "bars not split by account: trend sparkline, by model, by directory, quotas under 70%"
            ),
            &mut p.total,
        ),
        (t!("计费", "Billing"), "other", t!("没颜色的账号(「其他」)", "accounts without a color (“other”)"), &mut p.other),
        (t!("计费", "Billing"), "track", t!("额度条没用掉的那截", "the unused part of a quota bar"), &mut p.track),
        (t!("代码", "Code"), "syn_kw", t!("会话记录(Z)里代码的关键字", "keywords in code in the session record (Z)"), &mut p.syn_kw),
        (t!("代码", "Code"), "syn_str", t!("代码里的字符串", "strings in code"), &mut p.syn_str),
        (t!("代码", "Code"), "syn_num", t!("代码里的数字、常量", "numbers and constants in code"), &mut p.syn_num),
        (t!("代码", "Code"), "syn_fn", t!("代码里的函数名", "function names in code"), &mut p.syn_fn),
        (t!("代码", "Code"), "syn_ty", t!("代码里的类型名", "type names in code"), &mut p.syn_ty),
        (t!("代码", "Code"), "syn_com", t!("代码里的注释", "comments in code"), &mut p.syn_com),
        (
            t!("代码", "Code"),
            "add_bg",
            t!("会话记录的改动里加的那行的底色", "background of added lines in the session record's changes"),
            &mut p.add_bg,
        ),
        (
            t!("代码", "Code"),
            "del_bg",
            t!("会话记录的改动里删的那行的底色", "background of removed lines in the session record's changes"),
            &mut p.del_bg,
        ),
    ]
}

/// 钱花在哪那根条的五段,parts 按这个顺序写
const PARTS: [Tr; 5] = [
    Tr::new("输入", "input"),
    Tr::new("缓存写", "cache write"),
    Tr::new("缓存读", "cache read"),
    Tr::new("输出", "output"),
    Tr::new("搜索", "search"),
];
/// 星期 × 钟点的四档,heat 按这个顺序写
const HEAT: [Tr; 4] = [Tr::new("最少", "least"), Tr::new("少", "less"), Tr::new("多", "more"), Tr::new("最多", "most")];

const LIGHT: Pal = Pal {
    accent: Color::Rgb(0x88, 0x39, 0xEF),
    on_accent: Color::Rgb(0xFF, 0xFF, 0xFF),
    sel: Color::Rgb(0xE2, 0xD8, 0xF6),
    fade: Color::Rgb(0xBC, 0xC0, 0xCC),
    shadow: Color::Rgb(0xC9, 0xCD, 0xD8),
    minor: Color::Rgb(0x6C, 0x6F, 0x85),
    faint: Color::Rgb(0x8C, 0x8F, 0xA1),
    warn: Color::Rgb(0x9A, 0x5B, 0x00),
    hint: Color::Yellow,
    hint_typed: Color::DarkGray,
    on_hint: Color::Black,
    red: Color::Rgb(0xD2, 0x0F, 0x39),
    green: Color::Rgb(0x2E, 0x7D, 0x32),
    amber: Color::Rgb(0xB0, 0x6A, 0x00),
    muted: Color::Rgb(0x6C, 0x6F, 0x85),
    hunk: Color::Cyan,
    claude: Color::Rgb(0xC2, 0x41, 0x0C),
    codex: Color::Rgb(0x25, 0x63, 0xEB),
    series: &[
        Color::Rgb(0x59, 0x92, 0xE7),
        Color::Rgb(0xF0, 0x93, 0x62),
        Color::Rgb(0x51, 0xBF, 0xBB),
        Color::Rgb(0xC6, 0x6C, 0xA1),
        Color::Rgb(0x71, 0xB2, 0x62),
    ],
    total: Color::Rgb(0xB2, 0x9C, 0xE8),
    other: Color::Rgb(0xAC, 0xB0, 0xBE),
    track: Color::Rgb(0xDC, 0xDF, 0xE8),
    parts: [
        Color::Rgb(0xB6, 0x9E, 0xEE),
        Color::Rgb(0xA1, 0x80, 0xE7),
        Color::Rgb(0x8E, 0x61, 0xDF),
        Color::Rgb(0x7C, 0x42, 0xD3),
        Color::Rgb(0x66, 0x2C, 0xB5),
    ],
    heat: [Color::Rgb(0xD2, 0xC5, 0xF7), Color::Rgb(0xB9, 0xA0, 0xF4), Color::Rgb(0xA2, 0x7A, 0xEF), Color::Rgb(0x8C, 0x50, 0xE9)],
    syn_kw: Color::Rgb(0x88, 0x39, 0xEF),
    syn_str: Color::Rgb(0x2E, 0x7D, 0x32),
    syn_num: Color::Rgb(0xC2, 0x41, 0x0C),
    syn_fn: Color::Rgb(0x1D, 0x5F, 0xD9),
    syn_ty: Color::Rgb(0x12, 0x78, 0x80),
    syn_com: Color::Rgb(0x6C, 0x6F, 0x85),
    add_bg: Color::Rgb(0xDD, 0xEF, 0xDB),
    del_bg: Color::Rgb(0xF7, 0xDE, 0xE2),
};

const DARK: Pal = Pal {
    accent: Color::Rgb(0xCB, 0xA6, 0xF7),
    on_accent: Color::Rgb(0x1E, 0x1E, 0x2E),
    sel: Color::Rgb(0x3B, 0x34, 0x52),
    fade: Color::Rgb(0x58, 0x5B, 0x70),
    shadow: Color::Rgb(0x11, 0x11, 0x1B),
    minor: Color::Rgb(0xA6, 0xAD, 0xC8),
    faint: Color::Rgb(0x7F, 0x84, 0x9C),
    warn: Color::Rgb(0xF9, 0xE2, 0xAF),
    hint: Color::Yellow,
    hint_typed: Color::DarkGray,
    on_hint: Color::Black,
    red: Color::Rgb(0xF3, 0x8B, 0xA8),
    green: Color::Rgb(0xA6, 0xE3, 0xA1),
    amber: Color::Rgb(0xF9, 0xE2, 0xAF),
    muted: Color::Rgb(0xA6, 0xAD, 0xC8),
    hunk: Color::Cyan,
    claude: Color::Rgb(0xFA, 0xB3, 0x87),
    codex: Color::Rgb(0x89, 0xB4, 0xFA),
    series: &[
        Color::Rgb(0x58, 0x8C, 0xDA),
        Color::Rgb(0xCA, 0x77, 0x4A),
        Color::Rgb(0x21, 0x9B, 0xA1),
        Color::Rgb(0x9B, 0x4C, 0x7C),
        Color::Rgb(0x60, 0x9B, 0x53),
    ],
    total: Color::Rgb(0x92, 0x74, 0xC3),
    other: Color::Rgb(0x6C, 0x70, 0x86),
    track: Color::Rgb(0x31, 0x32, 0x44),
    parts: [
        Color::Rgb(0x60, 0x4B, 0x83),
        Color::Rgb(0x79, 0x5E, 0xA5),
        Color::Rgb(0x93, 0x72, 0xC8),
        Color::Rgb(0xB1, 0x8D, 0xE5),
        Color::Rgb(0xD1, 0xAF, 0xFA),
    ],
    heat: [Color::Rgb(0x48, 0x39, 0x62), Color::Rgb(0x6D, 0x53, 0x98), Color::Rgb(0x9B, 0x77, 0xCD), Color::Rgb(0xCB, 0xA6, 0xF7)],
    syn_kw: Color::Rgb(0xCB, 0xA6, 0xF7),
    syn_str: Color::Rgb(0xA6, 0xE3, 0xA1),
    syn_num: Color::Rgb(0xFA, 0xB3, 0x87),
    syn_fn: Color::Rgb(0x89, 0xB4, 0xFA),
    syn_ty: Color::Rgb(0x94, 0xE2, 0xD5),
    syn_com: Color::Rgb(0x93, 0x99, 0xB2),
    add_bg: Color::Rgb(0x2A, 0x3D, 0x33),
    del_bg: Color::Rgb(0x45, 0x29, 0x3A),
};

/// 内置的主题:(名字, 深色?, 颜色)
const BUILTIN: [(&str, bool, &Pal); 2] = [("latte", false, &LIGHT), ("mocha", true, &DARK)];

/// 一套主题
#[derive(Clone)]
pub struct Theme {
    pub name: String,
    pub dark: bool,
    pub pal: Pal,
    /// 主题文件;内置的是 None
    pub file: Option<PathBuf>,
    /// base 那套(`aca theme` 标出改过的);内置的是 None
    parent: Option<(String, Pal)>,
}

fn builtin(name: &str) -> Option<Theme> {
    BUILTIN.iter().find(|b| b.0 == name).map(|b| Theme { name: name.to_string(), dark: b.1, pal: *b.2, file: None, parent: None })
}

fn latte() -> Theme {
    builtin("latte").expect("内置")
}

/// 主题文件放哪:config.toml 旁边的 themes/(测试时跟着 ACA_CONFIG 走)
fn themes_dir() -> PathBuf {
    crate::config::path().parent().map_or_else(|| PathBuf::from("themes"), |d| d.join("themes"))
}

/// 名字能不能当文件名
fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains(['/', '\\'])
}

/// 按名字找:themes/ 里的文件优先(正在解析它自己时除外,比如 latte.toml 里的 base = "latte"),再是内置的。
/// seen:这一串 base 里已经在解析的文件,绕回来就当没有
fn resolve(name: &str, dir: &Path, seen: &mut Vec<String>, bad: &mut Vec<String>) -> Option<Theme> {
    let file = dir.join(format!("{name}.toml"));
    if !valid_name(name) || seen.iter().any(|s| s == name) || !file.is_file() {
        return builtin(name);
    }
    seen.push(name.to_string());
    let at = format!("themes/{name}.toml");
    let t = match std::fs::read_to_string(&file)
        .map_err(|e| e.to_string())
        .and_then(|s| s.parse::<toml::Table>().map_err(|e| e.message().trim().to_string()))
    {
        Ok(t) => t,
        Err(e) => {
            bad.push(tr!("{at} 读不了,当没有这个文件:{e}", "{at} can't be read, treated as missing: {e}"));
            return builtin(name);
        }
    };
    let base = match t.get("base") {
        None => "latte".to_string(),
        Some(toml::Value::String(s)) => s.trim().to_string(),
        Some(v) => {
            bad.push(tr!("{at} base = {v}:要写主题的名字,从 latte 改起", "{at} base = {v}: should be a theme name; starting from latte"));
            "latte".to_string()
        }
    };
    let parent = resolve(&base, dir, seen, bad).unwrap_or_else(|| {
        bad.push(tr!("{at} base = \"{base}\":没有这个主题,从 latte 改起", "{at} base = \"{base}\": no such theme; starting from latte"));
        latte()
    });
    let dark = match t.get("dark") {
        None => parent.dark,
        Some(toml::Value::Boolean(b)) => *b,
        Some(v) => {
            bad.push(tr!("{at} dark = {v}:写 true 或 false", "{at} dark = {v}: write true or false"));
            parent.dark
        }
    };
    let pal = apply(parent.pal, &t, &at, bad);
    Some(Theme { name: name.to_string(), dark, pal, file: Some(file), parent: Some((parent.name, parent.pal)) })
}

fn color(v: &toml::Value) -> Option<Color> {
    v.as_str()?.trim().parse().ok()
}

/// 在 base 的颜色上改。series 整组换掉(写几个就是几个位子),parts 从头往后改(可以只写前几个)
fn apply(mut p: Pal, over: &toml::Table, at: &str, bad: &mut Vec<String>) -> Pal {
    for (k, v) in over {
        let list = |v: &toml::Value, max: usize, bad: &mut Vec<String>| {
            let Some(a) = v.as_array().filter(|a| !a.is_empty() && a.len() <= max) else {
                bad.push(tr!(
                    "{at} {k}:要写成 [\"#RRGGBB\", …]{}",
                    "{at} {k}: write it as [\"#RRGGBB\", …]{}",
                    if max < usize::MAX { tr!(",最多 {max} 个", ", at most {max}") } else { String::new() }
                ));
                return None;
            };
            let got: Vec<Color> = a.iter().filter_map(color).collect();
            if got.len() < a.len() {
                bad.push(tr!("{at} {k}:里面有认不出的颜色,整组当没写", "{at} {k}: has a color it can't read; the whole list is ignored"));
                return None;
            }
            Some(got)
        };
        match k.as_str() {
            "base" | "dark" => {}
            "series" => {
                if let Some(c) = list(v, usize::MAX, bad) {
                    // 读一次主题漏一点点内存:只在打开、改了主题文件、打开主题列表的时候发生
                    p.series = c.leak();
                }
            }
            "parts" => {
                if let Some(c) = list(v, PARTS.len(), bad) {
                    p.parts[..c.len()].copy_from_slice(&c);
                }
            }
            "heat" => {
                if let Some(c) = list(v, HEAT.len(), bad) {
                    p.heat[..c.len()].copy_from_slice(&c);
                }
            }
            _ => match tokens(&mut p).into_iter().find(|x| x.1 == k) {
                Some((.., slot)) => match color(v) {
                    Some(c) => *slot = c,
                    None => bad.push(tr!("{at} {k} = {v}:认不出的颜色", "{at} {k} = {v}: unknown color")),
                },
                None => bad.push(tr!("{at} {k}:没有这个名字(aca theme 看有哪些)", "{at} {k}: no such name (aca theme lists them)")),
            },
        }
    }
    p
}

/// config.toml 的 [theme] name
fn configured(text: &str, bad: &mut Vec<String>) -> Option<String> {
    let t = match text.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => {
            if !text.trim().is_empty() {
                bad.push(tr!("config.toml 格式不对,整个当没写:{}", "config.toml is malformed, all of it ignored: {}", e.message().trim()));
            }
            return None;
        }
    };
    let th = t.get("theme")?.as_table()?;
    let mut name = None;
    for (k, v) in th {
        match (k.as_str(), v) {
            ("name", toml::Value::String(s)) => name = Some(s.trim().to_string()),
            _ => bad.push(tr!("config.toml [theme] {k}:只认 name = \"主题名\"", "config.toml [theme] {k}: only name = \"<theme>\"")),
        }
    }
    name
}

/// ACA_THEME 的 dark / light
fn alias(v: &str) -> String {
    match v.trim().to_ascii_lowercase().as_str() {
        "dark" => "mocha".into(),
        "light" => "latte".into(),
        _ => v.trim().to_string(),
    }
}

/// 用哪套:界面里挑过的(只在设了 ACA_THEME 的终端里才有)> ACA_THEME > config.toml > latte
fn pick(cfg: &str, dir: &Path, env: Option<&str>, chosen: Option<&str>) -> (Theme, Vec<String>) {
    let mut bad = vec![];
    let from_cfg = configured(cfg, &mut bad);
    let name = chosen
        .map(str::to_string)
        .or_else(|| env.filter(|v| !v.trim().is_empty()).map(alias))
        .or(from_cfg)
        .unwrap_or_else(|| "latte".into());
    let t = resolve(&name, dir, &mut vec![], &mut bad).unwrap_or_else(|| {
        bad.push(tr!("没有叫 {name} 的主题,用 latte", "no theme named {name}; using latte"));
        latte()
    });
    (t, bad)
}

/// 所有主题:浅色在前、深色在后,各自内置的在前、再按名字
fn all_in(dir: &Path, bad: &mut Vec<String>) -> Vec<Theme> {
    let mut names: Vec<String> = BUILTIN.iter().map(|b| b.0.to_string()).collect();
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".toml").map(str::to_string))
        .filter(|n| valid_name(n) && !names.contains(n))
        .collect();
    files.sort();
    names.extend(files);
    let mut v: Vec<Theme> = names.iter().filter_map(|n| resolve(n, dir, &mut vec![], bad)).collect();
    v.sort_by_key(|t| t.dark);
    v
}

static PAL: RwLock<Option<&'static Pal>> = RwLock::new(None);
/// 设了 ACA_THEME 的终端里在界面里挑的:这个 aca 就用它(不然存进 config.toml 后一重读又被 ACA_THEME 压回去)
static CHOSEN: Mutex<Option<String>> = Mutex::new(None);

#[cfg(test)]
thread_local! {
    /// 测试各跑各的线程:主题列表的预览只换这个线程看到的
    static TEST_PAL: std::cell::Cell<Option<&'static Pal>> = const { std::cell::Cell::new(None) };
}

fn env() -> Option<String> {
    std::env::var("ACA_THEME").ok().filter(|v| !v.trim().is_empty())
}

/// 现在该用的那套(测试里只用内置的 latte,不读本机的配置)
fn load() -> (Theme, Vec<String>) {
    if cfg!(test) {
        return (latte(), vec![]);
    }
    let text = std::fs::read_to_string(crate::config::path()).unwrap_or_default();
    let chosen = CHOSEN.lock().unwrap_or_else(|e| e.into_inner()).clone();
    pick(&text, &themes_dir(), env().as_deref(), chosen.as_deref())
}

pub fn pal() -> &'static Pal {
    #[cfg(test)]
    if let Some(p) = TEST_PAL.get() {
        return p;
    }
    if let Some(p) = *PAL.read().unwrap_or_else(|e| e.into_inner()) {
        return p;
    }
    let p: &'static Pal = Box::leak(Box::new(load().0.pal));
    *PAL.write().unwrap_or_else(|e| e.into_inner()) = Some(p);
    p
}

/// 整个界面换成这套(主题列表的预览)
pub fn set(p: &'static Pal) {
    #[cfg(test)]
    {
        TEST_PAL.set(Some(p));
        return;
    }
    #[allow(unreachable_code)]
    {
        *PAL.write().unwrap_or_else(|e| e.into_inner()) = Some(p);
    }
}

/// 测试里看某个主题文件画出来什么样(bill 的 show)
#[cfg(test)]
pub fn from_dir(name: &str, dir: &Path) -> Option<Pal> {
    resolve(name, dir, &mut vec![], &mut vec![]).map(|t| t.pal)
}

/// 现在用的那套叫什么
pub fn current() -> String {
    load().0.name
}

/// 主题列表列的:所有主题
pub fn list() -> Vec<Theme> {
    if cfg!(test) {
        return BUILTIN.iter().filter_map(|b| builtin(b.0)).collect();
    }
    all_in(&themes_dir(), &mut vec![])
}

/// 用这套:记进 config.toml 的 [theme] name(别的内容、注释原样留着)。Err 是给底栏的一句话
pub fn choose(name: &str) -> Result<(), String> {
    if cfg!(test) {
        return Ok(());
    }
    if env().is_some() {
        *CHOSEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(name.to_string());
    }
    crate::config::set("theme", "name", toml::Value::String(name.to_string()))
}

/// 把 [theme] name 改成 name(见 `config::with_value`)
#[cfg(test)]
fn with_name(text: &str, name: &str) -> Option<String> {
    crate::config::with_value(text, "theme", "name", &toml::Value::String(name.to_string()))
}

/// 设了 ACA_THEME:设置页在名字后面标出来(这个终端里听它的)
pub fn forced() -> bool {
    env().is_some()
}

/// config.toml 的 [theme]、themes/ 里的文件写错了的(当没写),设置页列出来
pub fn problems() -> Vec<String> {
    if cfg!(test) {
        return vec![];
    }
    let mut bad = load().1;
    all_in(&themes_dir(), &mut bad);
    bad.dedup();
    bad
}

/// config.toml 和 themes/ 里每个文件的修改时间、大小,用来看改没改
fn stamp() -> Vec<(PathBuf, Option<(SystemTime, u64)>)> {
    let one = |p: PathBuf| {
        let m = std::fs::metadata(&p).ok().and_then(|m| Some((m.modified().ok()?, m.len())));
        (p, m)
    };
    let mut v = vec![one(crate::config::path())];
    let mut files: Vec<PathBuf> = std::fs::read_dir(themes_dir()).into_iter().flatten().flatten().map(|e| e.path()).collect();
    files.sort();
    v.extend(files.into_iter().map(one));
    v
}

/// 主循环每一帧叫一次:最多一秒看一次 config.toml、themes/ 改没改,改了就重读(下一帧整屏用新颜色)。
/// 改了返回 true:config.toml 里别的设置(输入法)也跟着重读
pub fn refresh() -> bool {
    type Seen = Option<(Instant, Vec<(PathBuf, Option<(SystemTime, u64)>)>)>;
    static SEEN: Mutex<Seen> = Mutex::new(None);
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.as_ref().is_some_and(|s| s.0.elapsed() < Duration::from_secs(1)) {
        return false;
    }
    let now = stamp();
    let changed = seen.as_ref().is_some_and(|s| s.1 != now);
    *seen = Some((Instant::now(), now));
    if changed {
        set(Box::leak(Box::new(load().0.pal)));
    }
    changed
}

/// 次要的字
pub fn minor() -> Style {
    Style::new().fg(pal().minor)
}

/// 分隔符
pub fn faint() -> Style {
    Style::new().fg(pal().faint)
}

/// 提示 / 警告
pub fn warn() -> Style {
    Style::new().fg(pal().warn)
}

pub fn accent() -> Style {
    Style::new().fg(pal().accent)
}

/// 强调色底的小标签 / 当前按钮
pub fn chip() -> Style {
    Style::new().fg(pal().on_accent).bg(pal().accent).add_modifier(Modifier::BOLD)
}

// ---------- aca theme ----------

/// 色块:终端的颜色名用终端的 ANSI 底色,跟着终端的配色走
fn swatch(c: Color) -> String {
    let bg = match c {
        Color::Reset => return "      ".into(),
        Color::Rgb(r, g, b) => format!("48;2;{r};{g};{b}"),
        Color::Indexed(n) => format!("48;5;{n}"),
        Color::Black => "40".into(),
        Color::Red => "41".into(),
        Color::Green => "42".into(),
        Color::Yellow => "43".into(),
        Color::Blue => "44".into(),
        Color::Magenta => "45".into(),
        Color::Cyan => "46".into(),
        Color::Gray => "47".into(),
        Color::DarkGray => "100".into(),
        Color::LightRed => "101".into(),
        Color::LightGreen => "102".into(),
        Color::LightYellow => "103".into(),
        Color::LightBlue => "104".into(),
        Color::LightMagenta => "105".into(),
        Color::LightCyan => "106".into(),
        Color::White => "107".into(),
    };
    format!("\x1b[{bg}m      \x1b[0m")
}

/// 一行:色块、名字、值(和 base 不一样的标 *,后面写 base 里是什么)、用在哪
fn row(color: bool, name: &str, c: Color, was: Option<(&str, Color)>, about: &str) -> String {
    let changed = was.filter(|w| w.1 != c);
    let val = if changed.is_some() { format!("{c} *") } else { c.to_string() };
    let tail = changed.map_or(String::new(), |(b, w)| tr!("({b} 里 {w})", "({w} in {b})"));
    let sw = if color { swatch(c) } else { String::new() };
    format!("  {sw} {name:<12} {val:<10} {about}{tail}")
}

/// 路径里的家目录写成 ~
fn tilde(p: &Path) -> String {
    let s = p.display().to_string();
    let home = std::env::var("HOME").unwrap_or_default();
    match s.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => s,
    }
}

fn kind(t: &Theme) -> String {
    let dk = if t.dark { t!("深色", "dark") } else { t!("浅色", "light") };
    match &t.file {
        Some(f) => format!("{dk} · {}", tilde(f)),
        None => tr!("{dk} · 内置", "{dk} · built-in"),
    }
}

/// 一套主题的每种颜色
fn show(t: &Theme, now: &str, color: bool) {
    let using = if t.name == now { t!(" · 现在用的", " · in use") } else { "" };
    println!("{}", tr!("aca 的配色:{}({}{using})", "aca colors: {} ({}{using})", t.name, kind(t)));
    match &t.file {
        Some(f) => println!(
            "{}",
            tr!(
                "改 {},只写跟 base 不一样的;存盘后开着的 aca 一秒内换上",
                "edit {}, writing only what differs from base; open aca windows switch within a second of saving",
                tilde(f)
            )
        ),
        None => println!(
            "{}",
            tr!(
                "内置的不能改:aca theme new 我的 {} 复制一份出来改,再 aca theme use 我的",
                "built-ins can't be edited: aca theme new mine {} makes a copy to edit, then aca theme use mine",
                t.name
            )
        ),
    }
    let mut p = t.pal;
    let mut base = t.parent.as_ref().map(|x| x.1).unwrap_or(p);
    let bname = t.parent.as_ref().map_or("", |x| x.0.as_str());
    let olds: Vec<Color> = tokens(&mut base).into_iter().map(|x| *x.3).collect();
    let mut last = "";
    for ((group, name, about, c), old) in tokens(&mut p).into_iter().zip(olds) {
        if group != last {
            println!("\n{group}");
            last = group;
        }
        println!("{}", row(color, name, *c, t.parent.as_ref().map(|_| (bname, old)), about));
    }
    // 位子现在给了谁(slots.rs 记的)
    let owners = crate::state::state_file("colors").and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
    let owner = |i: usize| owners.lines().filter_map(|l| l.split_once('\t')).find(|x| x.0 == i.to_string()).map(|x| x.1.to_string());
    let changed = t.parent.is_some() && p.series != base.series;
    println!(
        "{}",
        tr!(
            "\n计费 · 账号的颜色 series{}:一个账号一个位子,第一次见到时给、之后不换;有几个颜色就有几个位子,多的账号画 other 的灰",
            "\nBilling · account colors series{}: one slot per account, given when first seen and then kept; as many slots as colors, extra accounts are drawn in other's gray",
            if changed { " *" } else { "" }
        )
    );
    for (i, &c) in p.series.iter().enumerate() {
        let who = owner(i).unwrap_or_else(|| t!("(空着)", "(free)").into());
        println!("{}", row(color, &format!("series {}", i + 1), c, None, &who));
    }
    if changed {
        println!("{}", tr!("  {bname} 里:{}", "  in {bname}: {}", base.series.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(" ")));
    }
    println!("{}", tr!("\n计费 · 钱花在哪那根条 parts", "\nBilling · the where-the-money-went bar, parts"));
    for (i, (&c, name)) in p.parts.iter().zip(PARTS).enumerate() {
        println!("{}", row(color, &format!("parts {}", i + 1), c, t.parent.as_ref().map(|_| (bname, base.parts[i])), &name));
    }
    println!("{}", tr!("\n计费 · 星期 × 钟点 heat", "\nBilling · weekday × hour, heat"));
    for (i, (&c, name)) in p.heat.iter().zip(HEAT).enumerate() {
        println!("{}", row(color, &format!("heat {}", i + 1), c, t.parent.as_ref().map(|_| (bname, base.heat[i])), &name));
    }
}

/// 所有主题一行一个:现在用的打 ●,后面一排色块(强调色、状态、账号的颜色)
fn show_list(v: &[Theme], now: &str, color: bool) {
    println!(
        "{}",
        tr!(
            "\n有这些(aca theme use 名字 换过去,aca theme 名字 看它的颜色):",
            "\nThemes (aca theme use NAME switches, aca theme NAME shows its colors):"
        )
    );
    let w = v.iter().map(|t| t.name.width()).max().unwrap_or(0);
    for t in v {
        let dot = if t.name == now { "●" } else { " " };
        let chips = if color {
            let p = &t.pal;
            let block = |c: Color| swatch(c).replace("      ", "  ");
            let mut s = block(p.accent);
            s.push(' ');
            for c in [p.red, p.green, p.amber] {
                s.push_str(&block(c));
            }
            s.push(' ');
            for &c in p.series {
                s.push_str(&block(c));
            }
            s + "  "
        } else {
            String::new()
        };
        println!("  {dot} {}{} {chips}{}", t.name, " ".repeat(w - t.name.width()), kind(t));
    }
}

/// `aca theme`:现在用的那套的颜色和所有主题;`aca theme <名字>` 看那套;
/// `aca theme use <名字>` 换过去;`aca theme new <名字> [从哪套]` 生成主题文件
pub fn run(args: &[String], color: bool) -> i32 {
    let dir = themes_dir();
    let mut bad = vec![];
    let (cur, cfg_bad) = load();
    bad.extend(cfg_bad);
    let all = all_in(&dir, &mut bad);
    let find = |n: &str| all.iter().find(|t| t.name == n).cloned();
    let code = match args.first().map(String::as_str) {
        None => {
            show(&cur, &cur.name, color);
            show_list(&all, &cur.name, color);
            0
        }
        Some("use") => match args.get(1).map(|n| (n, find(n))) {
            Some((_, Some(t))) => match choose(&t.name) {
                Ok(()) => {
                    let over = env().map_or(String::new(), |e| {
                        tr!(
                            "(这个终端设了 ACA_THEME={e},在这里打开的 aca 还是听它的)",
                            " (this terminal sets ACA_THEME={e}, so aca opened here still follows it)"
                        )
                    });
                    println!(
                        "{}",
                        tr!(
                            "换成 {}({});开着的 aca 一秒内换上{over}",
                            "switched to {} ({}); open aca windows switch within a second{over}",
                            t.name,
                            kind(&t)
                        )
                    );
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            },
            Some((n, None)) => {
                eprintln!("{}", tr!("没有叫 {n} 的主题", "no theme named {n}"));
                show_list(&all, &cur.name, color);
                1
            }
            None => {
                eprintln!("{}", tr!("aca theme use <名字>", "aca theme use <NAME>"));
                show_list(&all, &cur.name, color);
                2
            }
        },
        Some("new") => {
            let Some(name) = args.get(1).filter(|n| valid_name(n)) else {
                eprintln!(
                    "{}",
                    tr!("aca theme new <名字> [从哪套改起,默认现在用的]", "aca theme new <NAME> [FROM, default: the current one]")
                );
                return 2;
            };
            let from = args.get(2).cloned().unwrap_or_else(|| cur.name.clone());
            let Some(base) = find(&from) else {
                eprintln!("{}", tr!("没有叫 {from} 的主题", "no theme named {from}"));
                show_list(&all, &cur.name, color);
                return 1;
            };
            let path = dir.join(format!("{name}.toml"));
            if path.exists() {
                eprintln!("{}", tr!("{} 已经有了,直接改它", "{} already exists; edit it directly", tilde(&path)));
                return 1;
            }
            match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, template(name, &base))) {
                Ok(()) => {
                    println!(
                        "{}",
                        tr!(
                            "写好了 {}:把要改的那行前面的 # 去掉再改;用它:aca theme use {name}",
                            "wrote {}: remove the # in front of a line to change it; use it: aca theme use {name}",
                            tilde(&path)
                        )
                    );
                    0
                }
                Err(e) => {
                    eprintln!("{}", tr!("写不了 {}:{e}", "can't write {}: {e}", tilde(&path)));
                    1
                }
            }
        }
        Some(n) => match find(n) {
            Some(t) => {
                show(&t, &cur.name, color);
                0
            }
            None => {
                eprintln!(
                    "{}",
                    tr!(
                        "没有叫 {n} 的主题(aca theme use / new 是换主题、建主题)",
                        "no theme named {n} (aca theme use / new switch and create themes)"
                    )
                );
                show_list(&all, &cur.name, color);
                1
            }
        },
    };
    bad.dedup();
    if !bad.is_empty() {
        println!("{}", tr!("\n没用上(当没写):", "\nNot used (ignored):"));
        for b in &bad {
            println!("  {b}");
        }
    }
    code
}

/// `aca theme new` 生成的文件:base 指向那套,每种颜色一行、全注释掉(值是 base 里的),去掉 # 就是改它
fn template(name: &str, base: &Theme) -> String {
    let q = |c: Color| format!("\"{c}\"");
    let mut p = base.pal;
    let mut s = tr!(
        "# aca 的主题「{name}」:只写跟 base 不一样的 —— 把要改的那行前面的 # 去掉再改,存盘后开着的 aca 一秒内换上。\n\
         # 颜色写 \"#RRGGBB\"、终端的颜色名(red、lightblue、darkgray…)、0~255 的色号,或 \"reset\"。\n\
         # 用它:aca theme use {name};每种颜色现在是什么:aca theme {name}\n\n\
         base = {}\n# dark = {}\n",
        "# aca theme “{name}”: write only what differs from base — remove the # in front of a line to change it;\n\
         # open aca windows switch within a second of saving.\n\
         # Colors are \"#RRGGBB\", terminal color names (red, lightblue, darkgray…), color numbers 0~255, or \"reset\".\n\
         # Use it: aca theme use {name}; what each color is now: aca theme {name}\n\n\
         base = {}\n# dark = {}\n",
        toml::Value::String(base.name.clone()),
        base.dark
    );
    let mut last = "";
    for (group, key, about, c) in tokens(&mut p) {
        if group != last {
            s.push_str(&format!("\n# ---- {group} ----\n"));
            last = group;
        }
        s.push_str(&format!("# {key} = {}    # {about}\n", q(*c)));
    }
    let arr = |v: &[Color]| v.iter().map(|&c| q(c)).collect::<Vec<_>>().join(", ");
    let names = |v: &[Tr]| v.iter().map(|x| x.get()).collect::<Vec<_>>().join(t!("、", ", "));
    s.push_str(&tr!(
        "\n# 账号的颜色,一个账号一个位子;有几个颜色就有几个位子,多的账号画 other 的灰\n# series = [{}]\n\
         # 钱花在哪那根条:{}\n# parts = [{}]\n\
         # 星期 × 钟点,花得越多越深:{}\n# heat = [{}]\n",
        "\n# Account colors, one slot per account; as many slots as colors, extra accounts are drawn in other's gray\n\
         # series = [{}]\n\
         # The where-the-money-went bar: {}\n# parts = [{}]\n\
         # Weekday × hour, darker means more spent: {}\n# heat = [{}]\n",
        arr(p.series),
        names(&PARTS),
        arr(&p.parts),
        names(&HEAT),
        arr(&p.heat)
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aca-themes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for (n, t) in files {
            std::fs::write(d.join(n), t).unwrap();
        }
        d
    }

    #[test]
    fn a_theme_file_changes_only_what_it_names_on_top_of_its_base() {
        let d = dir(
            "base",
            &[
                (
                    "blue.toml",
                    "accent = \"#112233\"\nhint = \"lightblue\"\nseries = [\"#010203\", \"red\", \"7\"]\nparts = [\"#AABBCC\"]\nheat = [\"#010101\", \"#020202\"]\nacent = \"#000000\"\nsel = \"nope\"\n",
                ),
                ("night.toml", "base = \"mocha\"\nseries = [\"#010203\", \"oops\"]\n"),
                ("deeper.toml", "base = \"night\"\ndark = false\n"),
            ],
        );
        let mut bad = vec![];
        let t = resolve("blue", &d, &mut vec![], &mut bad).unwrap();
        assert!(!t.dark);
        assert_eq!((t.pal.accent, t.pal.hint, t.pal.sel), (Color::Rgb(0x11, 0x22, 0x33), Color::LightBlue, LIGHT.sel));
        assert_eq!(t.pal.series, &[Color::Rgb(1, 2, 3), Color::Red, Color::Indexed(7)]);
        assert_eq!((t.pal.parts[0], &t.pal.parts[1..]), (Color::Rgb(0xAA, 0xBB, 0xCC), &LIGHT.parts[1..]));
        assert_eq!((&t.pal.heat[..2], &t.pal.heat[2..]), (&[Color::Rgb(1, 1, 1), Color::Rgb(2, 2, 2)][..], &LIGHT.heat[2..]));
        assert_eq!(bad.len(), 2, "{bad:?}");
        assert!(bad.iter().any(|b| b.contains("acent")) && bad.iter().any(|b| b.contains("sel")));
        // base 一层层往上找;series 有一个写错整组不要;dark 可以改
        let mut bad = vec![];
        let t = resolve("deeper", &d, &mut vec![], &mut bad).unwrap();
        assert_eq!((t.dark, t.pal.accent, t.pal.series), (false, DARK.accent, DARK.series));
        assert_eq!(t.parent.as_ref().unwrap().0, "night");
        assert_eq!(bad.len(), 1, "{bad:?}");
        // 列表:浅色在前,内置的在前
        let names: Vec<String> = all_in(&d, &mut vec![]).into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["latte", "blue", "deeper", "mocha", "night"]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_file_can_shadow_a_builtin_and_loops_fall_back() {
        let d = dir(
            "loop",
            &[
                ("latte.toml", "base = \"latte\"\naccent = \"#123456\"\n"),
                ("a.toml", "base = \"b\"\n"),
                ("b.toml", "base = \"a\"\naccent = \"#654321\"\n"),
                ("broken.toml", "accent = \n"),
            ],
        );
        let mut bad = vec![];
        let t = resolve("latte", &d, &mut vec![], &mut bad).unwrap();
        assert_eq!((t.pal.accent, t.pal.sel), (Color::Rgb(0x12, 0x34, 0x56), LIGHT.sel));
        assert!(bad.is_empty(), "{bad:?}");
        // a → b → a 绕回来:当没有,从 latte 改起,说一声
        let t = resolve("a", &d, &mut vec![], &mut bad).unwrap();
        assert_eq!(t.pal.accent, Color::Rgb(0x65, 0x43, 0x21));
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(resolve("broken", &d, &mut vec![], &mut vec![]).is_none());
        assert!(resolve("../x", &d, &mut vec![], &mut vec![]).is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn which_theme_config_env_and_a_pick_decide() {
        let d = dir("pick", &[]);
        let cfg = "[theme]\nname = \"mocha\"\n";
        assert_eq!(pick("", &d, None, None).0.name, "latte");
        assert_eq!(pick(cfg, &d, None, None).0.name, "mocha");
        assert_eq!(pick(cfg, &d, Some("light"), None).0.name, "latte");
        assert_eq!(pick("", &d, Some("DARK"), None).0.name, "mocha");
        assert_eq!(pick(cfg, &d, Some("dark"), Some("latte")).0.name, "latte");
        let (t, bad) = pick("[theme]\nname = \"gone\"\nmode = \"dark\"\n", &d, None, None);
        assert_eq!((t.name.as_str(), bad.len()), ("latte", 2), "{bad:?}");
        let (t, bad) = pick("[theme\nbroken", &d, None, None);
        assert_eq!((t.name.as_str(), bad.len()), ("latte", 1));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn choosing_rewrites_only_the_theme_name() {
        assert_eq!(with_name("", "mocha").unwrap(), "[theme]\nname = \"mocha\"\n");
        let cfg = "# 我的设置\n[new_group]\ndefault_path = \"~/code\"  # 注释\n";
        assert_eq!(with_name(cfg, "mocha").unwrap(), format!("{cfg}\n[theme]\nname = \"mocha\"\n"));
        let cfg = "[theme] # 配色\nname = \"latte\"  # 旧的\n\n[new_group]\ndefault_path = \"~/code\"\n";
        let out = with_name(cfg, "我的 \"蓝\"").unwrap();
        assert!(out.starts_with("[theme] # 配色\nname = ") && out.ends_with("\n\n[new_group]\ndefault_path = \"~/code\"\n"), "{out}");
        assert_eq!(out.parse::<toml::Table>().unwrap()["theme"]["name"].as_str(), Some("我的 \"蓝\""));
        let cfg = "[new_group]\ndefault_path = \"x\"\n[theme]\n[other]\nname = \"not this\"\n";
        let out = with_name(cfg, "mocha").unwrap();
        assert_eq!(out, "[new_group]\ndefault_path = \"x\"\n[theme]\nname = \"mocha\"\n[other]\nname = \"not this\"\n");
        assert!(with_name("[theme\nbroken", "mocha").is_none());
    }

    #[test]
    fn a_new_theme_file_is_its_base_until_you_uncomment_a_line() {
        let base = builtin("mocha").unwrap();
        let text = template("我的", &base);
        let d = dir("tpl", &[("mine.toml", &text)]);
        let mut bad = vec![];
        let t = resolve("mine", &d, &mut vec![], &mut bad).unwrap();
        assert!(bad.is_empty(), "{bad:?}");
        assert!(t.dark);
        let (mut a, mut b) = (t.pal, DARK);
        let (x, y): (Vec<Color>, Vec<Color>) =
            (tokens(&mut a).into_iter().map(|x| *x.3).collect(), tokens(&mut b).into_iter().map(|x| *x.3).collect());
        assert_eq!(x, y);
        // 每种颜色都在里面,去掉 # 就是一行合法的设置
        let on = text
            .replace("# accent = ", "accent = ")
            .replace("# series = ", "series = ")
            .replace("# parts = ", "parts = ")
            .replace("# heat = ", "heat = ");
        std::fs::write(d.join("mine.toml"), on).unwrap();
        let t = resolve("mine", &d, &mut vec![], &mut bad).unwrap();
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!((t.pal.accent, t.pal.series, t.pal.parts, t.pal.heat), (DARK.accent, DARK.series, DARK.parts, DARK.heat));
        let mut p = DARK;
        for (_, k, ..) in tokens(&mut p) {
            assert!(text.contains(&format!("# {k} = ")), "{k}");
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn code_colors_are_tokens_a_theme_file_can_change() {
        let d = dir("code", &[("c.toml", "syn_kw = \"blue\"\nadd_bg = \"#010203\"\n")]);
        let mut bad = vec![];
        let t = resolve("c", &d, &mut vec![], &mut bad).unwrap();
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!((t.pal.syn_kw, t.pal.add_bg, t.pal.syn_str), (Color::Blue, Color::Rgb(1, 2, 3), LIGHT.syn_str));
        let mut p = LIGHT;
        let code: Vec<&str> = tokens(&mut p).iter().filter(|x| x.0 == "代码").map(|x| x.1).collect();
        assert_eq!(code, ["syn_kw", "syn_str", "syn_num", "syn_fn", "syn_ty", "syn_com", "add_bg", "del_bg"]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn every_token_has_a_distinct_name() {
        let mut p = LIGHT;
        let names: Vec<&str> = tokens(&mut p).iter().map(|x| x.1).collect();
        for (i, n) in names.iter().enumerate() {
            assert!(!names[i + 1..].contains(n), "{n}");
            assert!(!["series", "parts", "heat", "base", "dark"].contains(n));
        }
    }
}
