//! The text buffer: a list of lines plus a cursor and scroll offset.
//!
//! The cursor `x` coordinate is a *char index* into the line (not a byte
//! offset), so unicode text is handled without panicking.

use std::borrow::Cow;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use unicode_width::UnicodeWidthChar;

/// Byte index of the `idx`-th char in `s` (or `s.len()` if past the end).
fn char_index_to_byte(s: &str, idx: usize) -> usize {
    s.char_indices()
        .nth(idx)
        .map(|(byte, _)| byte)
        .unwrap_or(s.len())
}

/// Number of chars in `s`.
fn char_count(s: &str) -> usize {
    s.chars().count()
}

/// One level of indentation (Tab inserts this many spaces).
const INDENT_UNIT: &str = "    ";
/// Number of columns used when displaying a tab character.
pub(crate) const TAB_WIDTH: usize = INDENT_UNIT.len();

/// Width of a character in the editor's rendered text.
///
/// Ratatui deliberately skips control characters when writing a buffer, so a
/// literal tab would otherwise occupy no columns. Keep tabs in the editable
/// buffer, but render and lay them out as one regular indentation unit.
pub(crate) fn char_width(c: char) -> usize {
    if c == '\t' {
        TAB_WIDTH
    } else {
        c.width().unwrap_or(0)
    }
}

/// Replace tabs with their visible indentation while borrowing tab-free text.
pub(crate) fn expand_tabs(s: &str) -> Cow<'_, str> {
    if s.contains('\t') {
        Cow::Owned(s.replace('\t', &" ".repeat(TAB_WIDTH)))
    } else {
        Cow::Borrowed(s)
    }
}

/// The leading run of spaces/tabs of `s` (the line's indentation).
fn leading_whitespace(s: &str) -> &str {
    let end = s
        .char_indices()
        .find_map(|(i, c)| (!(c == ' ' || c == '\t')).then_some(i))
        .unwrap_or(s.len());
    &s[..end]
}

/// Chars one indent unit at the start of `line` takes: a leading tab, or
/// up to `INDENT_UNIT.len()` leading spaces (partial indents are
/// partially removed, e.g. two spaces on an oddly-indented line).
fn count_indent_unit(line: &str) -> usize {
    if line.starts_with('\t') {
        return 1;
    }
    line.chars()
        .take_while(|&c| c == ' ')
        .count()
        .min(INDENT_UNIT.len())
}

/// Remove one indent unit from the start of `line`; returns how many
/// chars were removed.
fn remove_indent_unit(line: &mut String) -> usize {
    let n = count_indent_unit(line);
    if n > 0 {
        line.drain(..n);
    }
    n
}

/// Maximum number of undo steps kept in memory (bounded history).
const MAX_UNDO: usize = 1000;

/// A saved pre-edit state, restored by undo/redo.
#[derive(Clone)]
struct Snapshot {
    lines: Vec<String>,
    cursor: (usize, usize),
    selection_anchor: Option<(usize, usize)>,
    selecting: bool,
    dirty: bool,
}

/// What kind of edit a snapshot belongs to; consecutive edits of the same
/// kind at a continuous cursor position merge into a single undo step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EditKind {
    InsertChar,
    Newline,
    Backspace,
    Delete,
    DeleteSelection,
    Indent,
    Dedent,
    /// Fallback if a Parinfer rewrite happens without a preceding user-edit
    /// snapshot. Cursor-only navigation never runs the engine and never
    /// produces this kind.
    Parinfer,
}

/// Index of the first line where `a` and `b` differ, or `None` when the
/// line lists are identical.
fn first_diff_line(a: &[String], b: &[String]) -> Option<usize> {
    let n = a.len().min(b.len());
    (0..n)
        .find(|&i| a[i] != b[i])
        .or_else(|| (a.len() != b.len()).then_some(n))
}

/// Map a `(char, line)` position from `old` lines onto `new` after a
/// Parinfer rewrite. Prefers an identity mapping when the line is
/// unchanged, then indent-only shifts, then clamping.
fn map_pos_across_lines(old: &[String], new: &[String], (x, y): (usize, usize)) -> (usize, usize) {
    if new.is_empty() {
        return (0, 0);
    }
    if y >= new.len() {
        let last = new.len() - 1;
        return (char_count(&new[last]), last);
    }
    let new_len = char_count(&new[y]);
    let Some(old_line) = old.get(y) else {
        return (x.min(new_len), y);
    };
    let new_line = &new[y];
    if old_line == new_line {
        return (x.min(new_len), y);
    }
    let old_indent = leading_whitespace(old_line).chars().count();
    let new_indent = leading_whitespace(new_line).chars().count();
    let old_rest = &old_line[leading_whitespace(old_line).len()..];
    let new_rest = &new_line[leading_whitespace(new_line).len()..];
    if old_rest == new_rest {
        let rel = x.saturating_sub(old_indent);
        return ((new_indent + rel).min(new_len), y);
    }
    (x.min(new_len), y)
}

pub struct Buffer {
    pub lines: Vec<String>,
    /// Cursor position: `(x, y)` where `x` is a char index.
    pub cursor: (usize, usize),
    /// Scroll offset: `(x, y)`. Without wrapping `y` is a logical line
    /// and `x` a char index; with wrapping `y` is a *visual* row (wrapped
    /// lines occupy several) and `x` is kept at 0.
    pub scroll: (usize, usize),
    /// Soft-wrap long lines at this many columns. `0` (or `wrap == false`)
    /// means no wrapping; the app refreshes this from the terminal width
    /// on every draw, so a resize re-wraps without any user action.
    pub wrap_width: usize,
    /// Whether long lines soft-wrap instead of scrolling horizontally.
    pub wrap: bool,
    /// File this buffer is bound to, if any.
    pub path: Option<PathBuf>,
    /// Whether the buffer has unsaved changes.
    pub dirty: bool,
    /// The content the buffer is considered clean against (the pristine
    /// state for unsaved buffers, the loaded/saved content otherwise).
    /// Used to recompute `dirty` after undo/redo, which snapshot-based
    /// dirty flags cannot capture across a save.
    clean_lines: Vec<String>,
    line_ending: &'static str,
    clean_disk_content: String,
    /// First line changed by the most recent edit operation (used to
    /// invalidate the syntax-highlight cache). Cleared by the app after
    /// use.
    pub last_edit_line: Option<usize>,
    /// Selection anchor `(line, char)`; `None` when nothing is selected.
    /// The selection spans `[anchor, cursor]` (normalized).
    pub selection_anchor: Option<(usize, usize)>,
    /// True while the user is extending the selection (mouse drag or
    /// shift+arrow).
    pub selecting: bool,
    /// Undo history: the state before each undo step, newest last.
    undo_stack: Vec<Snapshot>,
    /// Redo history, newest last; cleared by any new edit.
    redo_stack: Vec<Snapshot>,
    /// Kind and cursor position *after* the previous edit, used to merge
    /// consecutive same-kind edits (continuous typing, backspacing or
    /// deleting) into a single undo step. `None` after cursor movement,
    /// selection changes, saves, and undo/redo.
    last_edit: Option<(EditKind, (usize, usize))>,
    /// While set, every edit merges into the current undo step (a paste
    /// must be a single step even though it internally splits into
    /// several `insert_char`/`newline` calls).
    force_merge: bool,
    /// Buffer text last seen by Parinfer Smart Mode. Used as `prevText` so
    /// the engine can classify the user's change instead of guessing.
    parinfer_prev_text: String,
    /// Cursor last seen by Parinfer, as `(char_index, line)`.
    parinfer_prev_cursor: (usize, usize),
}

impl Buffer {
    /// A fresh, empty buffer not bound to any file.
    pub fn empty() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: (0, 0),
            scroll: (0, 0),
            wrap_width: 0,
            wrap: false,
            path: None,
            dirty: false,
            clean_lines: vec![String::new()],
            line_ending: "\n",
            clean_disk_content: String::new(),
            last_edit_line: None,
            selection_anchor: None,
            selecting: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: None,
            force_merge: false,
            parinfer_prev_text: String::new(),
            parinfer_prev_cursor: (0, 0),
        }
    }

    /// An empty buffer bound to `path`. The file is not created until the
    /// first save, so `rat new.txt` can open a path that does not exist yet.
    pub fn empty_at(path: PathBuf) -> Self {
        let mut buf = Self::empty();
        buf.path = Some(path);
        buf
    }

    fn line_len(&self, y: usize) -> usize {
        self.lines.get(y).map_or(0, |l| char_count(l))
    }

    /// Clamp the cursor x to the length of its line.
    fn clamp_x(&mut self) {
        let len = self.line_len(self.cursor.1);
        if self.cursor.0 > len {
            self.cursor.0 = len;
        }
    }

    /// Record that line `y` was changed; keeps the earliest changed line of
    /// the current operation (multi-line edits like paste touch several).
    fn mark_edited(&mut self, y: usize) {
        self.last_edit_line = Some(self.last_edit_line.map_or(y, |l| l.min(y)));
    }

    /// Buffer contents as a single string (`'\n'`-joined), matching how
    /// files are saved.
    pub(crate) fn content(&self) -> String {
        self.lines.join("\n")
    }
}

mod editing;
mod history;
mod navigation;
mod parinfer;
mod selection;
mod storage;
mod viewport;
mod wrapping;

#[cfg(test)]
use storage::atomic_save;
#[cfg(test)]
use wrapping::visual_chunks;
use wrapping::{snap_visible_back, snap_visible_fwd};
pub(crate) use wrapping::{visual_chunk, visual_len, visual_row_of};

#[cfg(test)]
mod tests;
