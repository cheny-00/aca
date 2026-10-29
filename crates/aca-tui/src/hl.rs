//! Code coloring: syntax definitions are bat's set (syntect syntaxes packaged by two-face, which know TOML, TypeScript and
//! the like); colors come only from the palette (`theme::Pal`'s syn_*, one set each for light / dark). Fed line by line;
//! state across lines (multi-line strings, comments) is kept in `Hl`.
//! A syntax is compiled the first time it's used (onig, 1-2 ms per language); coloring a line takes tens of microseconds
//! (a syntax is a pile of regexes matched line by line), so only what's visible on screen is colored (see `plain`).

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
    /// How many more lines can be colored (once used up, later lines stay as they are)
    static BUDGET: Cell<usize> = const { Cell::new(usize::MAX) };
}

/// Run without coloring (tens of microseconds a line, not worth it off screen): coloring doesn't change the text and wraps
/// into the same number of lines, so lay out uncolored first to measure each block's height, then color and relayout the
/// visible blocks
pub fn plain<R>(f: impl FnOnce() -> R) -> R {
    let was = NO_COLOR.replace(true);
    let r = f();
    NO_COLOR.set(was);
    r
}

/// Run with at most n lines colored: when only the top part of a block shows, the part below the screen isn't colored for now
/// (each piece of code has to be fed from the start, so only the tail can be saved)
pub fn upto<R>(n: usize, f: impl FnOnce() -> R) -> R {
    let was = BUDGET.replace(n);
    let r = f();
    BUDGET.set(was);
    r
}

/// Overlong lines (JSON squashed into one line, logs) aren't colored: a line of thousands of characters takes milliseconds,
/// and shows nothing useful anyway
const LONG: usize = 400;

fn syntaxes() -> &'static SyntaxSet {
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    S.get_or_init(two_face::syntax::extra_newlines)
}

/// The syntax theme records not a color but "which class" (r = the class number, a = 1): the color is taken from the current
/// palette only when drawing, so it follows a live theme change (`theme::set`), and terminal color names (blue, reset) in
/// the theme file work too. a = 0 = no class (variables, punctuation), uses the span's own color
const PLAIN: SColor = SColor { r: 0, g: 0, b: 0, a: 0 };

/// Which palette color class k uses
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
        // More specific selectors win (syntect compares them itself); operators and punctuation aren't colored (0)
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

/// The colorer for one piece of code
pub struct Hl {
    syn: &'static SyntaxReference,
    hl: Highlighter<'static>,
    parse: ParseState,
    state: HighlightState,
    /// The line fed to the syntax (with newline), reused to avoid allocating per line
    buf: String,
}

impl Hl {
    fn new(syn: &'static SyntaxReference) -> Option<Hl> {
        // Plain text needs no coloring
        if syn.name == "Plain Text" || NO_COLOR.get() {
            return None;
        }
        let hl = Highlighter::new(theme());
        let state = HighlightState::new(&hl, ScopeStack::new());
        Some(Hl { syn, hl, parse: ParseState::new(syn), state, buf: String::new() })
    }

    /// Detect the language from the file name: the extension, the whole name when there's no extension (Makefile,
    /// Dockerfile), then the first line (#!/bin/bash)
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

    /// Detect from the language a code block names (```rust, ```sh, ```ts)
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

    /// Start over (between two pieces of code that aren't contiguous, e.g. two hunks of a diff)
    pub fn reset(&mut self) {
        self.parse = ParseState::new(self.syn);
        self.state = HighlightState::new(&self.hl, ScopeStack::new());
    }

    /// Color one line (control characters already removed, Tabs turned into spaces). Parts with no color assigned use base
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
        // State across lines: the next line inside a multi-line comment is still comment
        let mut h = Hl::for_path("a.rs", None).unwrap();
        h.line("/* start", Style::new());
        assert_eq!(h.line("still comment */", Style::new())[0].style.fg, Some(p.syn_com));
        // Knows TOML, TypeScript, shell, the language a code block names; plain text isn't colored
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
        // Theme changed (preview, theme file edited): lines colored after that use the new colors; terminal color names work too
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
