//! 界面和命令行说哪种话:英文(默认)或中文。
//!
//! 短的字跟着用它的代码写,两种话并排:`tr!("没有会话 {key}", "no session matches {key}")`。
//! 两边都是 `format!` 的写法(`{key}`、`{:>w$}`、后面跟参数都照用),编译时两边都查。
//! 不用拼的一句用 `t!("界面", "Interface")`,是 `&'static str`;表里的、`const` 里的用 [`Tr`]。
//! 成篇的说明(按键页这种)一种话一份。
//! 没翻的地方(不该翻的,见下)照旧是中文。
//!
//! 句子要整句翻:拼起来的(`format!("分组上不能{what}")` + `"开窗口"`)两边的片段都得是那种话,
//! 英文的语序不一样,片段要连着整句一起看。
//!
//! 不翻的:给脚本读的输出(接到管道里的 `aca ls`)、写进文件 / tmux / agent 配置里的、拿来比对的字。
//! 逻辑别看显示出来的字(`who == "你"`):用枚举,显示时再变成字。
//!
//! 用哪种:环境变量 `ACA_LANG` > config.toml 的 `[ui] lang` > 英文。`zh…` / `en…` 都认(`en_US.UTF-8` 也行)。
//! 界面 `^S` 设置里按 `l` 当场换,也写进 config.toml;开着的别的 aca 看到 config.toml 改了跟着换。
//! 不看 `LANG` / `LC_ALL`:不少人 locale 是 en_US 但要中文,跟着它反倒要多设一次。

use std::cell::Cell;
use std::fmt;
use std::ops::Deref;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::paths;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

/// 这个进程用哪种:0 = 还没定(第一次用到时再定,hook 这种不说话的进程不用读配置)
static CHOSEN: AtomicU8 = AtomicU8::new(0);

thread_local! {
    /// 测试里临时换一种:只管这个线程,并行跑的别的测试不受影响
    static HERE: Cell<Option<Lang>> = const { Cell::new(None) };
}

pub fn lang() -> Lang {
    if let Some(l) = HERE.with(Cell::get) {
        return l;
    }
    match CHOSEN.load(Ordering::Relaxed) {
        1 => Lang::Zh,
        2 => Lang::En,
        _ => {
            let l = detect();
            set(l);
            l
        }
    }
}

/// 换成这种(设置页里改了马上生效)。在 [`with`] 里面只换这个线程的(测试里按设置页的键不碰别的测试)
pub fn set(l: Lang) {
    if HERE.with(|h| h.get().is_some()) {
        return HERE.with(|h| h.set(Some(l)));
    }
    CHOSEN.store(if l == Lang::Zh { 1 } else { 2 }, Ordering::Relaxed);
}

/// 这个终端用环境变量 `ACA_LANG` 定了(config.toml 改了不跟)
pub fn forced() -> bool {
    std::env::var("ACA_LANG").is_ok_and(|v| parse(&v).is_some())
}

/// config.toml 改了(设置页、编辑器、另一个 aca):照它换;`ACA_LANG` 定了的不动
pub fn reload() {
    if !forced() {
        set(saved().unwrap_or(Lang::En));
    }
}

/// 在这个线程里用 `l` 跑一段(测试用)
pub fn with<R>(l: Lang, f: impl FnOnce() -> R) -> R {
    let before = HERE.with(|h| h.replace(Some(l)));
    let r = f();
    HERE.with(|h| h.set(before));
    r
}

/// `zh` / `zh_CN.UTF-8` / `en` / `en-US`…;别的不认
pub fn parse(s: &str) -> Option<Lang> {
    let s = s.trim().to_ascii_lowercase();
    if s.starts_with("zh") {
        Some(Lang::Zh)
    } else if s.starts_with("en") {
        Some(Lang::En)
    } else {
        None
    }
}

fn detect() -> Lang {
    std::env::var("ACA_LANG").ok().and_then(|v| parse(&v)).or_else(saved).unwrap_or(Lang::En)
}

/// config.toml 的 `[ui] lang`
fn saved() -> Option<Lang> {
    let text = std::fs::read_to_string(paths::config_dir().join("config.toml")).ok()?;
    parse(text.parse::<toml::Table>().ok()?.get("ui")?.get("lang")?.as_str()?)
}

impl Lang {
    /// 写进 config.toml、设置页上显示的
    pub fn code(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
        }
    }
}

/// 英文的单复数(中文没有,只给英文那边用):`let w = noun(n, "call", "calls"); tr!("{n} 次调用", "{n} {w}")`
pub fn noun(n: u64, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

/// 一句两种话,给表和 `const` 用。用的时候当 `&str`(`&*x`、`x.lines()`)或者直接 `{x}`,就是现在那种
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tr {
    pub zh: &'static str,
    pub en: &'static str,
}

impl Tr {
    pub const fn new(zh: &'static str, en: &'static str) -> Tr {
        Tr { zh, en }
    }

    /// 两种话写法一样的(`show <id>`)
    pub const fn same(s: &'static str) -> Tr {
        Tr { zh: s, en: s }
    }

    pub fn get(self) -> &'static str {
        match lang() {
            Lang::Zh => self.zh,
            Lang::En => self.en,
        }
    }
}

impl Deref for Tr {
    type Target = str;
    fn deref(&self) -> &str {
        self.get()
    }
}

impl fmt::Display for Tr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.get())
    }
}

/// `tr!("中文", "English")`、`tr!("共 {n} 个", "{n} in all")`、`tr!("{} 个", "{} items", n)`:现在那种话的 `format!`
#[macro_export]
macro_rules! tr {
    ($zh:literal, $en:literal $(,)?) => {
        match $crate::i18n::lang() {
            $crate::i18n::Lang::Zh => ::std::format!($zh),
            $crate::i18n::Lang::En => ::std::format!($en),
        }
    };
    ($zh:literal, $en:literal, $($args:tt)+) => {
        match $crate::i18n::lang() {
            $crate::i18n::Lang::Zh => ::std::format!($zh, $($args)+),
            $crate::i18n::Lang::En => ::std::format!($en, $($args)+),
        }
    };
}

/// `t!("界面", "Interface")`:不用拼的一句,`&'static str`(原来写字面量的地方都能放)
#[macro_export]
macro_rules! t {
    ($zh:literal, $en:literal $(,)?) => {
        $crate::i18n::Tr::new($zh, $en).get()
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_are_format_strings_and_the_thread_can_pick_one() {
        let (what, n, w) = ("x", 3, 4);
        let both =
            || (tr!("分组上不能{what}", "can't {what} on a group"), tr!("{n:>w$} 个", "{n:>w$} in all"), tr!("{} 个", "{} items", n));
        assert_eq!(with(Lang::Zh, both), ("分组上不能x".into(), "   3 个".into(), "3 个".into()));
        assert_eq!(with(Lang::En, both), ("can't x on a group".into(), "   3 in all".into(), "3 items".into()));
        let k = Tr::new("界面", "Interface");
        assert_eq!((with(Lang::Zh, || k.to_string()), with(Lang::En, || k.len())), ("界面".into(), 9));
        // 出了 with 回到原来的;里面 set 只换这个线程的
        with(Lang::En, || with(Lang::Zh, || ()));
        assert_eq!(HERE.with(Cell::get), None);
        assert_eq!(with(Lang::Zh, || (set(Lang::En), lang()).1), Lang::En);
        assert_eq!(HERE.with(Cell::get), None);
    }

    #[test]
    fn locale_names_are_recognized_by_their_start() {
        assert_eq!(
            (parse("zh"), parse("zh_CN.UTF-8"), parse(" EN-us "), parse("en")),
            (Some(Lang::Zh), Some(Lang::Zh), Some(Lang::En), Some(Lang::En))
        );
        assert_eq!((parse("ja"), parse(""), parse("C")), (None, None, None));
    }
}
