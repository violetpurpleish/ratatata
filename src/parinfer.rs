//! Parinfer Smart Mode for Clojure-family buffers.
//!
//! Ratatata runs the vendored parinfer-rust engine after each user edit
//! (and after cursor movement) when the open file is `.clj`, `.cljs`,
//! `.cljc`, or `.edn`. Other file types are untouched. The engine is
//! fail-safe: a parse error (for example an unclosed string while typing)
//! leaves the user's text as they typed it.

use std::path::Path;

use unicode_width::UnicodeWidthChar;

use crate::parinfer_engine::parinfer;
use crate::parinfer_engine::types::{Options, Request};

/// File extensions that enable Parinfer Smart Mode.
const PARINFER_EXTENSIONS: &[&str] = &["clj", "cljs", "cljc", "edn"];

/// Whether Parinfer should run for a buffer bound to `path`.
pub fn applies_to_path(path: Option<&Path>) -> bool {
    path.and_then(|p| p.extension())
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            PARINFER_EXTENSIONS
                .iter()
                .any(|want| ext.eq_ignore_ascii_case(want))
        })
}

/// One Parinfer Smart Mode request.
pub struct Input<'a> {
    pub text: &'a str,
    /// Cursor as `(char_index, line)`.
    pub cursor: (usize, usize),
    pub prev_text: &'a str,
    pub prev_cursor: (usize, usize),
    pub selection_start_line: Option<usize>,
}

/// Successful Smart Mode result. On failure the caller keeps the input.
pub struct Output {
    pub text: String,
    /// Cursor as `(char_index, line)` in `text`.
    pub cursor: (usize, usize),
}

/// Run Smart Mode. Returns `None` when the engine reports an error so the
/// caller can keep the user's in-progress text.
pub fn smart_edit(input: &Input<'_>) -> Option<Output> {
    let (cx, cy) = input.cursor;
    let line = nth_line(input.text, cy);
    let (pcx, pcy) = input.prev_cursor;
    let prev_line = nth_line(input.prev_text, pcy);

    let request = Request {
        mode: "smart".to_string(),
        text: input.text.to_string(),
        options: Options {
            cursor_x: Some(char_index_to_column(line, cx)),
            cursor_line: Some(cy),
            prev_cursor_x: Some(char_index_to_column(prev_line, pcx)),
            prev_cursor_line: Some(pcy),
            prev_text: Some(input.prev_text.to_string()),
            selection_start_line: input.selection_start_line,
            ..Options::default()
        },
    };
    let answer = parinfer::process(&request);
    if !answer.success {
        return None;
    }
    let text = answer.text.clone().into_owned();
    let cursor_line = answer.cursor_line.unwrap_or(cy);
    let cursor_col = answer.cursor_x.unwrap_or(0);
    let cursor_x = column_to_char_index(nth_line(&text, cursor_line), cursor_col);
    Some(Output {
        text,
        cursor: (cursor_x, cursor_line),
    })
}

fn nth_line(text: &str, y: usize) -> &str {
    text.split('\n').nth(y).unwrap_or("")
}

/// Parinfer `x` coordinates are unicode display columns, not char indices.
fn char_index_to_column(line: &str, char_idx: usize) -> usize {
    line.chars()
        .take(char_idx)
        .map(|c| c.width().unwrap_or(0))
        .sum()
}

fn column_to_char_index(line: &str, column: usize) -> usize {
    let mut col = 0;
    for (i, c) in line.chars().enumerate() {
        if col >= column {
            return i;
        }
        col += c.width().unwrap_or(0);
    }
    line.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(text: &str, cursor: (usize, usize), prev: &str, prev_cursor: (usize, usize)) -> Output {
        smart_edit(&Input {
            text,
            cursor,
            prev_text: prev,
            prev_cursor,
            selection_start_line: None,
        })
        .expect("parinfer should succeed")
    }

    #[test]
    fn clojure_family_extensions_enable_parinfer() {
        for name in ["a.clj", "a.cljs", "a.cljc", "a.edn", "A.CLJ"] {
            assert!(applies_to_path(Some(Path::new(name))), "{name}");
        }
        for name in ["a.txt", "a.rs", "a.clojure", "clj", ".clj"] {
            assert!(!applies_to_path(Some(Path::new(name))), "{name}");
        }
        assert!(!applies_to_path(None));
    }

    #[test]
    fn typing_an_open_paren_inserts_its_match() {
        let out = edit("(", (1, 0), "", (0, 0));
        assert_eq!(out.text, "()");
        assert_eq!(out.cursor, (1, 0));
    }

    #[test]
    fn indenting_a_line_moves_the_closing_paren() {
        let out = edit("(foo)\n    bar", (4, 1), "(foo)\nbar", (0, 1));
        assert_eq!(out.text, "(foo\n    bar)");
        assert_eq!(out.cursor, (4, 1));
    }

    #[test]
    fn unclosed_string_is_left_alone() {
        assert!(
            smart_edit(&Input {
                text: "\"hello",
                cursor: (6, 0),
                prev_text: "",
                prev_cursor: (0, 0),
                selection_start_line: None,
            })
            .is_none()
        );
    }

    #[test]
    fn incomplete_form_stays_editable() {
        let out = edit("(def foo", (8, 0), "", (0, 0));
        assert_eq!(out.text, "(def foo)");
        assert_eq!(out.cursor, (8, 0));
    }

    #[test]
    fn column_conversion_round_trips_ascii() {
        assert_eq!(char_index_to_column("abc", 2), 2);
        assert_eq!(column_to_char_index("abc", 2), 2);
    }
}
