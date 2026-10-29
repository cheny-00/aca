//! Which language the interface and the CLI speak: English (the default) or Chinese.
//!
//! Short text is written next to the code that uses it, both languages side by side:
//! `tr!("没有会话 {key}", "no session matches {key}")`.
//! Both sides are `format!` strings (`{key}`, `{:>w$}` and trailing arguments all work as usual), and both are checked at compile
//! time. A sentence with nothing to put together uses `t!("界面", "Interface")`, a `&'static str`; ones in tables and `const`s
//! use [`Tr`]. Long texts (like the keys page) have one copy per language.
//! What isn't translated (what shouldn't be, see below) stays Chinese.
//!
//! Translate whole sentences: when pieces are put together (`format!("分组上不能{what}")` + `"开窗口"`), the pieces on both sides
//! must be in that language, and since English word order differs, look at the pieces together with the whole sentence.
//!
//! Not translated: output for scripts to read (`aca ls` piped somewhere), what's written into files / tmux / agent configuration,
//! and text used for comparison. Logic shouldn't look at displayed text (`who == "你"`): use an enum and turn it into text only
//! for display.
//!
//! Which one: the `ACA_LANG` environment variable > `[ui] lang` in config.toml > English. Both `zh…` / `en…` are accepted
//! (`en_US.UTF-8` too). Pressing `l` in the interface's `^S` settings switches on the spot and also writes config.toml; other
//! open aca instances switch along when they see config.toml change.
//! `LANG` / `LC_ALL` aren't consulted: plenty of people have an en_US locale but want Chinese, and following it would just mean one
//! more thing to set.

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

/// Which one this process uses: 0 = not decided yet (decided at first use, so silent processes like hooks don't read the config)
static CHOSEN: AtomicU8 = AtomicU8::new(0);

thread_local! {
    /// Switched temporarily in tests: only this thread, other tests running in parallel aren't affected
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

/// Switch to this one (a change on the Settings page takes effect at once). Inside [`with`], only this thread's (pressing Settings
/// page keys in a test doesn't touch other tests)
pub fn set(l: Lang) {
    if HERE.with(|h| h.get().is_some()) {
        return HERE.with(|h| h.set(Some(l)));
    }
    CHOSEN.store(if l == Lang::Zh { 1 } else { 2 }, Ordering::Relaxed);
}

/// This terminal fixed it with the `ACA_LANG` environment variable (doesn't follow changes to config.toml)
pub fn forced() -> bool {
    std::env::var("ACA_LANG").is_ok_and(|v| parse(&v).is_some())
}

/// config.toml changed (Settings page, an editor, another aca): switch to match; left alone when `ACA_LANG` fixes it
pub fn reload() {
    if !forced() {
        set(saved().unwrap_or(Lang::En));
    }
}

/// Run a piece of code in this thread with `l` (for tests)
pub fn with<R>(l: Lang, f: impl FnOnce() -> R) -> R {
    let before = HERE.with(|h| h.replace(Some(l)));
    let r = f();
    HERE.with(|h| h.set(before));
    r
}

/// `zh` / `zh_CN.UTF-8` / `en` / `en-US`…; nothing else is accepted
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

/// `[ui] lang` in config.toml
fn saved() -> Option<Lang> {
    let text = std::fs::read_to_string(paths::config_dir().join("config.toml")).ok()?;
    parse(text.parse::<toml::Table>().ok()?.get("ui")?.get("lang")?.as_str()?)
}

impl Lang {
    /// What's written into config.toml and shown on the Settings page
    pub fn code(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
        }
    }
}

/// English singular / plural (Chinese has none, so it's only for the English side):
/// `let w = noun(n, "call", "calls"); tr!("{n} 次调用", "{n} {w}")`
pub fn noun(n: u64, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

/// One sentence in two languages, for tables and `const`s. Use it as a `&str` (`&*x`, `x.lines()`) or straight as `{x}`, and it's
/// the current language
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tr {
    pub zh: &'static str,
    pub en: &'static str,
}

impl Tr {
    pub const fn new(zh: &'static str, en: &'static str) -> Tr {
        Tr { zh, en }
    }

    /// Written the same in both languages (`show <id>`)
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

/// `tr!("中文", "English")`, `tr!("共 {n} 个", "{n} in all")`, `tr!("{} 个", "{} items", n)`: `format!` in the current language
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

/// `t!("界面", "Interface")`: a sentence with nothing to put together, `&'static str` (fits anywhere a literal used to go)
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
        // Leaving with goes back to what it was; set inside it only switches this thread's
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
