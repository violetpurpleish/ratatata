//! Syntax highlighting via syntect (Sublime Text grammars + themes).
//!
//! Highlighting is incremental: the parse state and the scope stack after
//! each line are cached, so after an edit only the lines from the edit
//! point onward are re-parsed — and only when they become visible. This is
//! the caching pattern recommended in syntect's own `HighlightState` docs.
//!
//! The heavy assets (bundled syntax definitions and the Catppuccin Mocha theme
//! from `two-face`) are loaded once into process-wide statics; this also
//! sidesteps the lifetime problem of `syntect::highlighting::Highlighter`,
//! which borrows its `Theme`.

use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style as TuiStyle};
use syntect::highlighting::{
    FontStyle, HighlightState, Highlighter as SyntectHighlighter, RangedHighlightIterator, Style,
    Theme,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use two_face::theme::{EmbeddedLazyThemeSet, EmbeddedThemeName};

fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    static SET: OnceLock<EmbeddedLazyThemeSet> = OnceLock::new();
    SET.get_or_init(two_face::theme::extra)
        .get(EmbeddedThemeName::CatppuccinMocha)
}

pub struct Highlighter {
    syntax_set: &'static SyntaxSet,
    highlighter: SyntectHighlighter<'static>,
    syntax: SyntaxReference,
    /// Parse state after line `i` (`parse_states[i]`).
    parse_states: Vec<ParseState>,
    /// Highlight scope stack after line `i`.
    scope_stacks: Vec<ScopeStack>,
    /// Styled byte ranges for each highlighted line. Keeping the rendered
    /// ranges alongside the parser state is important because the app draws
    /// repeatedly even when the viewport did not change (for example, when
    /// the mouse wheel is already at a scroll boundary).
    highlighted_lines: Vec<Vec<(Option<TuiStyle>, Range<usize>)>>,
}

impl Highlighter {
    pub fn new() -> Self {
        let syntax_set = syntax_set();
        Self {
            syntax_set,
            highlighter: SyntectHighlighter::new(theme()),
            syntax: syntax_set.find_syntax_plain_text().clone(),
            parse_states: Vec::new(),
            scope_stacks: Vec::new(),
            highlighted_lines: Vec::new(),
        }
    }

    /// Name of the currently detected syntax (e.g. "Rust", "Plain Text").
    pub fn syntax_name(&self) -> &str {
        &self.syntax.name
    }

    /// Re-detect the syntax for `path` (by extension, then first-line
    /// heuristics such as shebangs) and drop the highlight cache.
    pub fn set_path(&mut self, path: Option<&Path>) {
        let syntax = match path {
            Some(path) => {
                // by extension + first-line heuristics (reads the file)
                let by_file = self.syntax_set.find_syntax_for_file(path).ok().flatten();
                // fallback for files that don't exist yet: extension only
                let by_ext = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .and_then(|ext| self.syntax_set.find_syntax_by_extension(ext));
                by_file
                    .or(by_ext)
                    .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text())
            }
            None => self.syntax_set.find_syntax_plain_text(),
        };
        self.syntax = syntax.clone();
        self.invalidate_from(0);
    }

    /// Drop cached state from `line` onward (call after editing `line`).
    pub fn invalidate_from(&mut self, line: usize) {
        self.parse_states.truncate(line);
        self.scope_stacks.truncate(line);
        self.highlighted_lines.truncate(line);
    }

    /// Highlight line `y` of `lines`. Returns styled byte ranges covering
    /// the whole line contiguously; `None` means "plain" (the theme's
    /// default style, rendered with the terminal's default colors).
    ///
    /// Highlighted ranges are cached with the parser state, so drawing an
    /// already-highlighted line does not parse it again. If `y` is ahead of
    /// the cache — for example, after an edit above the viewport — the gap is
    /// re-parsed lazily from the buffer.
    pub fn highlight_line(
        &mut self,
        lines: &[String],
        y: usize,
    ) -> &[(Option<TuiStyle>, Range<usize>)] {
        if y >= lines.len() {
            return &[];
        }
        while self.parse_states.len() <= y {
            let line_y = self.parse_states.len();
            self.highlight_one(&lines[line_y], line_y);
        }
        &self.highlighted_lines[y]
    }

    /// Highlight one line, given that `parse_states.len() == y`.
    fn highlight_one(&mut self, line: &str, y: usize) {
        let (mut parse_state, initial_stack) = if y == 0 {
            (ParseState::new(&self.syntax), ScopeStack::new())
        } else {
            (
                self.parse_states[y - 1].clone(),
                self.scope_stacks[y - 1].clone(),
            )
        };

        let ops = parse_state
            .parse_line(line, self.syntax_set)
            .unwrap_or_default();
        let mut highlight_state = HighlightState::new(&self.highlighter, initial_stack);
        let iter =
            RangedHighlightIterator::new(&mut highlight_state, &ops, line, &self.highlighter);
        let mut out: Vec<(Option<TuiStyle>, Range<usize>)> = iter
            .map(|(style, _text, range)| (self.map_style(style), range))
            .collect();

        // the iterator covers the whole line, but guard against a partial
        // parse leaving a gap at the end
        if let Some((_, last)) = out.last()
            && last.end < line.len()
        {
            out.push((None, last.end..line.len()));
        }

        self.parse_states.push(parse_state);
        self.scope_stacks.push(highlight_state.path.clone());
        self.highlighted_lines.push(out);
    }

    /// Map a syntect style to a ratatui style. The theme's default style
    /// maps to `None` so plain text keeps the terminal's default colors.
    fn map_style(&self, style: Style) -> Option<TuiStyle> {
        if style == self.highlighter.get_default() {
            return None;
        }
        let mut out = TuiStyle::default();
        if style.foreground.a > 0 {
            out = out.fg(Color::Rgb(
                style.foreground.r,
                style.foreground.g,
                style.foreground.b,
            ));
        }
        if style.font_style.contains(FontStyle::BOLD) {
            out = out.add_modifier(Modifier::BOLD);
        }
        if style.font_style.contains(FontStyle::ITALIC) {
            out = out.add_modifier(Modifier::ITALIC);
        }
        if style.font_style.contains(FontStyle::UNDERLINE) {
            out = out.add_modifier(Modifier::UNDERLINED);
        }
        Some(out)
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!("ratatata-hl-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn highlighter(path: &str) -> Highlighter {
        let mut h = Highlighter::new();
        h.set_path(Some(Path::new(path)));
        h
    }

    fn lines_of(content: &str) -> Vec<String> {
        content.split('\n').map(str::to_string).collect()
    }

    fn styled_ranges<'a>(
        h: &'a mut Highlighter,
        lines: &[String],
        y: usize,
    ) -> &'a [(Option<TuiStyle>, Range<usize>)] {
        h.highlight_line(lines, y)
    }

    #[test]
    fn detects_rust_by_extension() {
        let h = highlighter("src/main.rs");
        assert_eq!(h.syntax_name(), "Rust");
    }

    #[test]
    fn detects_python_by_shebang() {
        let dir = scratch("shebang");
        let file = dir.join("tool");
        std::fs::write(&file, "#!/usr/bin/env python3\nprint('hi')\n").unwrap();
        let mut h = Highlighter::new();
        h.set_path(Some(&file));
        assert_eq!(h.syntax_name(), "Python");
    }

    #[test]
    fn unknown_extension_is_plain_text() {
        let h = highlighter("data.unknownext");
        assert_eq!(h.syntax_name(), "Plain Text");
    }

    #[test]
    fn no_path_is_plain_text() {
        let mut h = Highlighter::new();
        h.set_path(None);
        assert_eq!(h.syntax_name(), "Plain Text");
        // and a nonexistent new file still gets Rust by extension
        h.set_path(Some(Path::new("/does/not/exist/yet.rs")));
        assert_eq!(h.syntax_name(), "Rust");
    }

    #[test]
    fn rust_keywords_and_strings_are_styled() {
        let mut h = highlighter("main.rs");
        let lines = lines_of("fn main() {\n    let msg = \"hi\";\n}\n");
        let ops0 = styled_ranges(&mut h, &lines, 0);
        // "fn" (byte 0..2) must be a styled keyword
        let fn_style = ops0.iter().find(|(_, r)| r.start == 0 && r.end == 2);
        assert!(fn_style.is_some_and(|(s, _)| s.is_some()));

        let ops1 = styled_ranges(&mut h, &lines, 1);
        // string content is styled (quotes themselves are not)
        let q = lines[1].find('"').unwrap();
        let content = ops1
            .iter()
            .find(|(_, r)| r.start == q + 1 && r.end == q + 3);
        assert!(content.is_some_and(|(s, _)| s.is_some()));
    }

    #[test]
    fn block_comment_state_carries_across_lines() {
        let mut h = highlighter("main.rs");
        let lines = lines_of("/* start\nstill comment\nend */ let x = 1;\n");
        // line 0 starts the comment
        styled_ranges(&mut h, &lines, 0);
        // line 1 is inside the comment -> styled (comment color)
        let ops1 = styled_ranges(&mut h, &lines, 1);
        assert!(ops1.iter().all(|(s, _)| s.is_some()));

        // the same text as the first line of a fresh buffer is plain
        let mut h2 = highlighter("main.rs");
        let first = lines_of("still comment\n");
        let ops_first = styled_ranges(&mut h2, &first, 0);
        assert!(ops_first.iter().all(|(s, _)| s.is_none()));

        // line 2: the comment ends, so the "let" keyword is styled again
        let ops2 = styled_ranges(&mut h, &lines, 2);
        let let_start = lines[2].find("let").unwrap();
        let let_style = ops2
            .iter()
            .find(|(_, r)| r.start == let_start && r.end == let_start + 3);
        assert!(let_style.is_some_and(|(s, _)| s.is_some()));
    }

    #[test]
    fn coverage_is_contiguous_and_complete() {
        let mut h = highlighter("main.rs");
        let lines = lines_of("fn main() {\n    let s = \"héllo\";\n}\n");
        for y in 0..lines.len() {
            let ops = h.highlight_line(&lines, y);
            let mut line_pos = 0;
            for (_, r) in ops {
                assert_eq!(r.start, line_pos, "gap on line {y}");
                line_pos = r.end;
            }
            assert_eq!(line_pos, lines[y].len(), "line {y} not fully covered");
        }
    }

    #[test]
    fn repeated_highlight_uses_cached_ranges() {
        let mut h = highlighter("main.rs");
        let lines = lines_of("fn main() {}\n");
        let first = h.highlight_line(&lines, 0).to_vec();

        for _ in 0..100 {
            assert_eq!(h.highlight_line(&lines, 0), first.as_slice());
        }

        // Re-rendering the same viewport must not append parser state or
        // re-run the syntax highlighter for the already cached line.
        assert_eq!(h.parse_states.len(), 1);
        assert_eq!(h.scope_stacks.len(), 1);
        assert_eq!(h.highlighted_lines.len(), 1);
    }

    #[test]
    fn invalidate_truncates_and_backfills() {
        let mut h = highlighter("main.rs");
        let lines: Vec<String> = (0..20).map(|i| format!("fn f{i:02}() {{}}")).collect();

        for y in 0..5 {
            styled_ranges(&mut h, &lines, y);
        }
        assert_eq!(h.parse_states.len(), 5);

        // editing line 2 invalidates from there
        h.invalidate_from(2);
        assert_eq!(h.parse_states.len(), 2);
        styled_ranges(&mut h, &lines, 2);
        assert_eq!(h.parse_states.len(), 3);

        // an edit above the viewport backfills the gap on demand
        for y in 3..7 {
            styled_ranges(&mut h, &lines, y);
        }
        assert_eq!(h.parse_states.len(), 7);
        h.invalidate_from(0);
        let ops = styled_ranges(&mut h, &lines, 7);
        assert!(!ops.is_empty());
        assert_eq!(h.parse_states.len(), 8);
    }

    #[test]
    fn plain_text_has_no_styles() {
        let mut h = highlighter("notes.txt");
        let lines = lines_of("just some words\n");
        let ops = styled_ranges(&mut h, &lines, 0);
        assert!(ops.iter().all(|(s, _)| s.is_none()));
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].1, 0..15);
    }

    #[test]
    fn out_of_range_line_is_empty() {
        let mut h = highlighter("main.rs");
        let lines = lines_of("fn main() {}\n");
        assert!(h.highlight_line(&lines, 5).is_empty());
    }
}
