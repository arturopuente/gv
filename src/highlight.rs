//! Whole-blob syntax highlighting (syntect, M1) into per-line, balanced HTML
//! using CSS classes, so light/dark themes are plain stylesheets.

use std::fmt::Write;
use std::sync::OnceLock;
use syntect::highlighting::ThemeSet;
use syntect::html::{ClassStyle, css_for_theme_with_class_style, line_tokens_to_classed_spans};
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

const STYLE: ClassStyle = ClassStyle::SpacedPrefixed { prefix: "s-" };
pub const MAX_LINES: usize = 20_000;
pub const MAX_LINE_CHARS: usize = 3_000;

fn syntaxes() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(two_face::syntax::extra_newlines)
}

/// Warm the syntax set on a background thread so the first fragment is fast.
pub fn warm() {
    std::thread::spawn(|| {
        syntaxes();
    });
}

fn find(path: &str, first_line: &str) -> Option<&'static SyntaxReference> {
    let ss = syntaxes();
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or(name);
    ss.find_syntax_by_extension(name)
        .or_else(|| ss.find_syntax_by_extension(ext))
        .or_else(|| ss.find_syntax_by_first_line(first_line))
        .filter(|s| s.name != "Plain Text")
}

/// Expand tabs to spaces at `tw`-column stops. Rendering and the client's
/// wrap math both see the expanded text, so widths agree.
pub fn expand_tabs(s: &str, tw: usize) -> std::borrow::Cow<'_, str> {
    if !s.contains('\t') {
        return s.into();
    }
    let mut out = String::with_capacity(s.len() + 16);
    let mut col = 0;
    for ch in s.chars() {
        if ch == '\t' {
            let n = tw - (col % tw);
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(ch);
            col += if ch == '\n' {
                0
            } else {
                unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0)
            };
        }
    }
    out.into()
}

/// Highlight a whole blob. Returns one HTML string per line (no trailing
/// newline), or None when the file is too large or has no known syntax.
pub fn highlight(path: &str, text: &str, tab_width: usize) -> Option<Vec<String>> {
    let lines = text.lines().count();
    if lines > MAX_LINES || text.lines().any(|l| l.len() > MAX_LINE_CHARS) {
        return None;
    }
    let text: String = text
        .lines()
        .map(|l| expand_tabs(l.trim_end_matches('\r'), tab_width).into_owned() + "\n")
        .collect();
    let syntax = find(path, text.lines().next().unwrap_or(""))?;
    let ss = syntaxes();
    let mut ps = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out = Vec::with_capacity(lines);
    for line in LinesWithEndings::from(&text) {
        let ops = ps.parse_line(line, ss).ok()?;
        let before: Vec<Scope> = stack.as_slice().to_vec();
        let (body, delta) = line_tokens_to_classed_spans(line, &ops, STYLE, &mut stack).ok()?;
        let mut html = String::with_capacity(body.len() + before.len() * 24);
        for scope in &before {
            html.push_str("<span class=\"");
            push_classes(&mut html, *scope);
            html.push_str("\">");
        }
        html.extend(body.chars().filter(|&c| c != '\n'));
        let open = before.len() as isize + delta;
        for _ in 0..open.max(0) {
            html.push_str("</span>");
        }
        out.push(html);
    }
    Some(out)
}

fn push_classes(s: &mut String, scope: Scope) {
    for (i, atom) in scope.build_string().split('.').enumerate() {
        if i > 0 {
            s.push(' ');
        }
        let _ = write!(s, "s-{atom}");
    }
}

/// Theme CSS: light by default, dark under prefers-color-scheme (unless the
/// page forces light) or when the page forces dark.
pub fn theme_css() -> &'static str {
    static CSS: OnceLock<String> = OnceLock::new();
    CSS.get_or_init(|| {
        let ts = ThemeSet::load_defaults();
        let light = css_for_theme_with_class_style(&ts.themes["InspiredGitHub"], STYLE).unwrap_or_default();
        let dark = css_for_theme_with_class_style(&ts.themes["base16-ocean.dark"], STYLE).unwrap_or_default();
        let scope = |css: &str, sel: &str| -> String {
            // Prefix every rule with `sel` so the two themes can coexist.
            css.split('}')
                .filter(|r| r.contains('{'))
                .map(|r| {
                    let (sels, body) = r.split_once('{').unwrap();
                    let sels: Vec<String> = sels
                        .split(',')
                        // Drop leading comments that got glued to the first selector.
                        .map(|s| s.rsplit("*/").next().unwrap_or(s).trim())
                        .filter(|s| !s.is_empty())
                        .map(|s| format!("{sel} {s}"))
                        .collect();
                    format!("{} {{{}}}\n", sels.join(", "), body)
                })
                .collect()
        };
        // Each theme applies only in its own mode, so a scope one theme leaves
        // uncolored never inherits the other theme's color.
        format!(
            "@media (prefers-color-scheme: light) {{\n{}}}\n{}\n@media (prefers-color-scheme: dark) {{\n{}}}\n{}",
            scope(&light, ":root:not([data-theme=dark])"),
            scope(&light, ":root[data-theme=light]"),
            scope(&dark, ":root:not([data-theme=light])"),
            scope(&dark, ":root[data-theme=dark]"),
        )
    })
}
