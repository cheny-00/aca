//! 代码上色:语法定义用 bat 那一套(two-face 打包的 syntect 语法,认得 TOML、TypeScript 这些),
//! 颜色只用调色板里的(`theme::Pal` 的 syn_*,浅色 / 深色各一套)。按行喂,跨行的状态(多行字符串、注释)记在 `Hl` 里。
//! 语法第一次用到时才编译(onig,一种语言 1~2 毫秒);上色一行几十微秒(语法是一堆正则逐行匹配),
//! 所以只给屏幕上看得见的上色(见 `plain`)。

use std::cell::Cell;
use std::sync::OnceLock;

use ratatui::style::{Color, Style};
use syntect::highlighting::{
    Color as SColor, HighlightIterator, HighlightState, Highlighter, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

use crate::ansi::{Seg, Styled};
use crate::theme;

thread_local! {
    static NO_COLOR: Cell<bool> = const { Cell::new(false) };
    /// 还能上色几行(用完了后面的行照原样)
    static BUDGET: Cell<usize> = const { Cell::new(usize::MAX) };
}

/// 不上色地做(一行几十微秒,屏幕外的不值得):上色不改字,折出来的行一样多,
/// 所以先不上色地排、量出每块多高,看得见的块再上色重排
pub fn plain<R>(f: impl FnOnce() -> R) -> R {
    let was = NO_COLOR.replace(true);
    let r = f();
    NO_COLOR.set(was);
    r
}

/// 最多上色 n 行地做:一块只露出上面一截时,屏幕底下那截先不上色(每段代码得从头喂,只能省后面的)
pub fn upto<R>(n: usize, f: impl FnOnce() -> R) -> R {
    let was = BUDGET.replace(n);
    let r = f();
    BUDGET.set(was);
    r
}

/// 太长的一行(压成一行的 JSON、日志)不上色:一行上千字要几毫秒,也看不出什么
const LONG: usize = 400;

fn syntaxes() -> &'static SyntaxSet {
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    S.get_or_init(two_face::syntax::extra_newlines)
}

/// 语法主题里记的不是颜色,是「哪一类」(r = 类的编号,a = 1):画的时候才从现在的调色板取颜色,
/// 所以主题实时换(`theme::set`)时跟着变,主题文件里写终端的颜色名(blue、reset)也行。a = 0 = 没这一类(变量、标点),用段落本来的颜色
const PLAIN: SColor = SColor { r: 0, g: 0, b: 0, a: 0 };

/// 第 k 类用调色板的哪个颜色
fn slot(p: &theme::Pal, k: u8) -> Option<Color> {
    Some(match k {
        1 => p.syn_com,
        2 => p.syn_str,
        3 => p.syn_num,
        4 => p.syn_kw,
        5 => p.syn_fn,
        6 => p.syn_ty,
        7 => p.green,
        8 => p.red,
        _ => return None,
    })
}

fn theme() -> &'static Theme {
    static T: OnceLock<Theme> = OnceLock::new();
    T.get_or_init(|| {
        // 越具体的选择器越优先(syntect 自己比);运算符、标点不上色(0)
        let (com, string, num, kw, func, ty, ins, del) = (1, 2, 3, 4, 5, 6, 7, 8);
        let rules: [(&str, u8); 20] = [
            ("comment, punctuation.definition.comment", com),
            ("string, punctuation.definition.string", string),
            ("constant.numeric, constant.language, constant.character, constant.other, support.constant", num),
            ("variable.language, variable.other.constant", num),
            ("keyword, storage, keyword.control, storage.type, storage.modifier", kw),
            ("entity.name.function, support.function, variable.function, meta.function-call.identifier", func),
            ("entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.name.trait, entity.name.interface", ty),
            ("support.type, support.class, entity.other.inherited-class, storage.type.primitive", ty),
            ("entity.name.tag", func),
            ("entity.other.attribute-name", num),
            ("entity.name.namespace, entity.name.module", ty),
            ("meta.attribute, meta.annotation, entity.name.label, meta.preprocessor", com),
            ("markup.heading, entity.name.section", kw),
            ("markup.raw, markup.inline.raw", string),
            ("markup.underline.link, markup.link", func),
            ("markup.inserted", ins),
            ("markup.deleted", del),
            ("meta.diff.range, meta.diff.header, meta.separator.diff", com),
            ("keyword.operator, punctuation.separator, punctuation.terminator, punctuation.accessor", 0),
            ("support.function.builtin.shell, support.function.builtin", func),
        ];
        let scopes = rules
            .iter()
            .filter_map(|(sel, k)| {
                let fg = if *k == 0 { PLAIN } else { SColor { r: *k, g: 0, b: 0, a: 1 } };
                Some(ThemeItem {
                    scope: sel.parse::<ScopeSelectors>().ok()?,
                    style: StyleModifier { foreground: Some(fg), background: None, font_style: None },
                })
            })
            .collect();
        Theme { settings: ThemeSettings { foreground: Some(PLAIN), ..ThemeSettings::default() }, scopes, ..Theme::default() }
    })
}

/// 一段代码的上色器
pub struct Hl {
    syn: &'static SyntaxReference,
    hl: Highlighter<'static>,
    parse: ParseState,
    state: HighlightState,
    /// 喂给语法的那行(带换行),接着用免得每行分配
    buf: String,
}

impl Hl {
    fn new(syn: &'static SyntaxReference) -> Option<Hl> {
        // 纯文本不用上色
        if syn.name == "Plain Text" || NO_COLOR.get() {
            return None;
        }
        let hl = Highlighter::new(theme());
        let state = HighlightState::new(&hl, ScopeStack::new());
        Some(Hl { syn, hl, parse: ParseState::new(syn), state, buf: String::new() })
    }

    /// 按文件名认语言:扩展名,没有扩展名的认整个名字(Makefile、Dockerfile),再认第一行(#!/bin/bash)
    pub fn for_path(path: &str, first: Option<&str>) -> Option<Hl> {
        if NO_COLOR.get() {
            return None;
        }
        let ss = syntaxes();
        let name = path.rsplit('/').next().unwrap_or(path);
        let ext = name.rsplit_once('.').map(|(_, e)| e);
        let syn = ext
            .and_then(|e| ss.find_syntax_by_extension(e))
            .or_else(|| ss.find_syntax_by_extension(name))
            .or_else(|| first.and_then(|l| ss.find_syntax_by_first_line(l)))?;
        Hl::new(syn)
    }

    /// 按代码块写的语言认(```rust、```sh、```ts)
    pub fn for_lang(token: &str) -> Option<Hl> {
        if NO_COLOR.get() {
            return None;
        }
        let t = token.trim().trim_start_matches('{').split([' ', ',', '}']).next().unwrap_or("").to_ascii_lowercase();
        if t.is_empty() {
            return None;
        }
        let ss = syntaxes();
        let t = match t.as_str() {
            "shell" | "console" | "zsh" => "sh",
            "text" | "txt" | "plain" | "plaintext" | "output" => return None,
            x => x,
        };
        Hl::new(ss.find_syntax_by_token(t)?)
    }

    /// 从头来(两段不连着的代码中间,比如改动的两处)
    pub fn reset(&mut self) {
        self.parse = ParseState::new(self.syn);
        self.state = HighlightState::new(&self.hl, ScopeStack::new());
    }

    /// 上色一行(已经去掉控制字符、Tab 换成空格的)。没配颜色的部分用 base
    pub fn line(&mut self, text: &str, base: Style) -> Styled {
        let left = BUDGET.get();
        if left == 0 || text.len() > LONG {
            return vec![Seg { text: text.to_string(), style: base, link: None }];
        }
        BUDGET.set(left - 1);
        self.buf.clear();
        self.buf.push_str(text);
        self.buf.push('\n');
        let Ok(ops) = self.parse.parse_line(&self.buf, syntaxes()) else {
            return vec![Seg { text: text.to_string(), style: base, link: None }];
        };
        let p = theme::pal();
        let mut out: Styled = vec![];
        for (st, piece) in HighlightIterator::new(&mut self.state, &ops, &self.buf, &self.hl) {
            let piece = piece.trim_end_matches('\n');
            if piece.is_empty() {
                continue;
            }
            let style = match slot(p, st.foreground.r) {
                Some(c) if st.foreground.a == 1 => base.fg(c),
                _ => base,
            };
            match out.last_mut() {
                Some(s) if s.style == style => s.text.push_str(piece),
                _ => out.push(Seg { text: piece.to_string(), style, link: None }),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_gets_palette_colors_and_the_rest_keeps_its_style() {
        let p = theme::pal();
        let mut h = Hl::for_path("src/a.rs", None).unwrap();
        let segs = h.line("fn main() { let s = \"hi\"; } // done", Style::new());
        let color = |w: &str| segs.iter().find(|s| s.text.contains(w)).map(|s| s.style.fg);
        assert_eq!(color("fn"), Some(Some(p.syn_kw)), "{segs:?}");
        assert_eq!(color("main"), Some(Some(p.syn_fn)), "{segs:?}");
        assert_eq!(color("\"hi\""), Some(Some(p.syn_str)), "{segs:?}");
        assert_eq!(color("// done"), Some(Some(p.syn_com)), "{segs:?}");
        assert_eq!(segs.iter().map(|s| s.text.as_str()).collect::<String>(), "fn main() { let s = \"hi\"; } // done");
        // 跨行的状态:多行注释里的下一行还是注释
        let mut h = Hl::for_path("a.rs", None).unwrap();
        h.line("/* start", Style::new());
        assert_eq!(h.line("still comment */", Style::new())[0].style.fg, Some(p.syn_com));
        // 认得 TOML、TypeScript、shell、代码块写的语言;纯文本不上色
        for path in ["Cargo.toml", "a.ts", "x.tsx", "run.sh", "Makefile", "Dockerfile"] {
            assert!(Hl::for_path(path, None).is_some(), "{path}");
        }
        assert!(Hl::for_path("script", Some("#!/bin/bash")).is_some());
        for lang in ["rust", "sh", "bash", "console", "py", "json", "diff", "ts"] {
            assert!(Hl::for_lang(lang).is_some(), "{lang}");
        }
        assert!(Hl::for_lang("text").is_none() && Hl::for_lang("").is_none() && Hl::for_path("a.txt", None).is_none());
    }

    #[test]
    fn colors_come_from_the_theme_in_use_right_now() {
        let mut h = Hl::for_path("a.rs", None).unwrap();
        assert_eq!(h.line("fn a() {}", Style::new())[0].style.fg, Some(theme::pal().syn_kw));
        // 换了主题(预览、主题文件改了):接着上色的行用新颜色;终端的颜色名也行
        let mut p = *theme::pal();
        p.syn_kw = Color::Blue;
        theme::set(Box::leak(Box::new(p)));
        assert_eq!(h.line("fn b() {}", Style::new())[0].style.fg, Some(Color::Blue));
    }

    #[test]
    fn plain_skips_coloring_upto_stops_after_n_lines_and_long_lines_stay_plain() {
        assert!(plain(|| Hl::for_path("a.rs", None)).is_none());
        assert!(Hl::for_path("a.rs", None).is_some(), "出了 plain 就恢复");
        let colored = |segs: &Styled| segs.iter().any(|s| s.style.fg.is_some());
        let got = upto(1, || {
            let mut h = Hl::for_path("a.rs", None).unwrap();
            (h.line("let a = 1;", Style::new()), h.line("let b = 2;", Style::new()))
        });
        assert!(colored(&got.0) && !colored(&got.1), "{got:?}");
        let mut h = Hl::for_path("a.rs", None).unwrap();
        let long = format!("let s = \"{}\";", "x".repeat(LONG));
        assert_eq!(h.line(&long, Style::new()), vec![Seg { text: long.clone(), style: Style::new(), link: None }]);
    }
}
