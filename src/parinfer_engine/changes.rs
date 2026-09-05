//! Vendored from [parinfer-rust](https://github.com/eraserhd/parinfer-rust) (Jason Felice).
//! ISC License: Copyright (c) 2018, Jason Felice and Contributors.
//!
//! Upstream commit: `0d334950099cd30a8e1ca520026f8da9ea1382fa`
//!
//! `compute_text_changes` differs from that commit in one place: the change
//! coordinate `x` is a *display column* (grapheme `UnicodeWidthStr` width,
//! matching the rest of the engine) rather than `+1` per Rust `char`. The
//! common-prefix / common-suffix byte ranges are unchanged.

use super::types::*;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub fn compute_text_changes<'a>(prev_text: &'a str, text: &'a str) -> Vec<Change> {
    let mut x: Column = 0;
    let mut line_no: LineNumber = 0;
    let mut start_text: usize = 0;
    let mut start_prev: usize = 0;
    let mut end_text: usize = text.len();
    let mut end_prev: usize = prev_text.len();
    let mut different: bool = false;

    for ((i, pc), (j, c)) in prev_text.char_indices().zip(text.char_indices()) {
        if pc != c {
            start_prev = i;
            start_text = j;
            different = true;
            break;
        }
    }

    if different {
        // Display column of the first difference. Upstream counted +1 per
        // scalar; a wide grapheme before the edit would then disagree with
        // the engine's grapheme-column `x`.
        for g in prev_text[..start_prev].graphemes(true) {
            if g == "\n" {
                x = 0;
                line_no += 1;
            } else {
                x += UnicodeWidthStr::width(g);
            }
        }
    }

    for ((i, pc), (j, c)) in prev_text
        .char_indices()
        .rev()
        .zip(text.char_indices().rev())
    {
        if pc != c || i < start_prev || j < start_text {
            end_prev = i + pc.len_utf8();
            end_text = j + c.len_utf8();
            break;
        }
    }

    if different {
        vec![Change {
            x,
            line_no,
            old_text: String::from(&prev_text[start_prev..end_prev]),
            new_text: String::from(&text[start_text..end_text]),
        }]
    } else {
        vec![]
    }
}

#[cfg(test)]
#[test]
fn compute_text_changes_works() {
    assert!(compute_text_changes("hello", "hello").is_empty());
    assert_eq!(
        vec![Change {
            x: 2,
            line_no: 0,
            old_text: String::from("l"),
            new_text: String::from("x")
        }],
        compute_text_changes("hello", "hexlo")
    );
    assert_eq!(
        vec![Change {
            x: 0,
            line_no: 1,
            old_text: String::from("l"),
            new_text: String::from("x")
        }],
        compute_text_changes("he\nllo", "he\nxlo")
    );
    assert_eq!(
        vec![Change {
            x: 4,
            line_no: 0,
            old_text: String::from(""),
            new_text: String::from("l")
        }],
        compute_text_changes("hello", "helllo")
    );
    assert_eq!(
        vec![Change {
            x: 4,
            line_no: 0,
            old_text: String::from("l"),
            new_text: String::from("")
        }],
        compute_text_changes("helllo", "hello")
    );
}

#[cfg(test)]
#[test]
fn compute_text_changes_x_is_display_column_after_wide_graphemes() {
    assert_eq!(
        compute_text_changes("界x", "界y"),
        vec![Change {
            x: 2,
            line_no: 0,
            old_text: String::from("x"),
            new_text: String::from("y"),
        }]
    );
    assert_eq!(
        compute_text_changes("e\u{0301}x", "e\u{0301}y"),
        vec![Change {
            x: 1,
            line_no: 0,
            old_text: String::from("x"),
            new_text: String::from("y"),
        }]
    );
    assert_eq!(
        compute_text_changes("👩\u{200D}💻x", "👩\u{200D}💻y"),
        vec![Change {
            x: 2,
            line_no: 0,
            old_text: String::from("x"),
            new_text: String::from("y"),
        }]
    );
}
