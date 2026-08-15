//! The text buffer: a list of lines plus a cursor and scroll offset.
//!
//! The cursor `x` coordinate is a *char index* into the line (not a byte
//! offset), so unicode text is handled without panicking.

use std::fs;
use std::io;
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
}

/// Index of the first line where `a` and `b` differ, or `None` when the
/// line lists are identical.
fn first_diff_line(a: &[String], b: &[String]) -> Option<usize> {
    let n = a.len().min(b.len());
    (0..n)
        .find(|&i| a[i] != b[i])
        .or_else(|| (a.len() != b.len()).then_some(n))
}

pub struct Buffer {
    pub lines: Vec<String>,
    /// Cursor position: `(x, y)` where `x` is a char index.
    pub cursor: (usize, usize),
    /// Scroll offset: `(x, y)` where `x` is a char index.
    pub scroll: (usize, usize),
    /// File this buffer is bound to, if any.
    pub path: Option<PathBuf>,
    /// Whether the buffer has unsaved changes.
    pub dirty: bool,
    /// The content the buffer is considered clean against (the pristine
    /// state for unsaved buffers, the loaded/saved content otherwise).
    /// Used to recompute `dirty` after undo/redo, which snapshot-based
    /// dirty flags cannot capture across a save.
    clean_lines: Vec<String>,
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
}

impl Buffer {
    /// A fresh, empty buffer not bound to any file.
    pub fn empty() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: (0, 0),
            scroll: (0, 0),
            path: None,
            dirty: false,
            clean_lines: vec![String::new()],
            last_edit_line: None,
            selection_anchor: None,
            selecting: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: None,
            force_merge: false,
        }
    }

    /// Load a file into a new buffer. Splitting on `'\n'` and joining with
    /// `'\n'` on save round-trips files byte-for-byte, with or without a
    /// trailing newline (a trailing `'\n'` yields a final empty line).
    pub fn from_path(path: PathBuf) -> io::Result<Self> {
        let content = fs::read_to_string(&path)?;
        let mut buf = Self::empty();
        buf.lines = content.split('\n').map(str::to_string).collect();
        buf.clean_lines = buf.lines.clone();
        buf.path = Some(path);
        Ok(buf)
    }

    /// Write the buffer back to its file.
    pub fn save(&mut self) -> io::Result<()> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "buffer has no file name"))?;
        let content = self.lines.join("\n");
        fs::write(path, content)?;
        self.clean_lines = self.lines.clone();
        self.dirty = false;
        // typing right after a save starts a fresh undo step
        self.last_edit = None;
        Ok(())
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

    // ---- editing -----------------------------------------------------------

    pub fn insert_char(&mut self, c: char) {
        let replaced = self.delete_selection();
        if !replaced && !self.mergeable(EditKind::InsertChar) {
            self.push_undo(EditKind::InsertChar);
        }
        let (x, y) = self.cursor;
        let line = &mut self.lines[y];
        let byte = char_index_to_byte(line, x);
        line.insert(byte, c);
        self.cursor.0 += 1;
        self.last_edit = Some((EditKind::InsertChar, self.cursor));
        self.dirty = true;
        self.mark_edited(y);
    }

    pub fn insert_text(&mut self, text: &str) {
        for c in text.chars() {
            self.insert_char(c);
        }
    }

    /// Insert possibly multi-line text (paste) at the cursor. A leading
    /// selection is replaced. The whole paste is a single undo step.
    pub fn insert_multiline(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        // open the paste's undo step explicitly, then fold the internal
        // insert_char/newline calls into it via force_merge
        if self.has_selection() {
            self.push_undo(EditKind::DeleteSelection);
        } else {
            self.push_undo(EditKind::InsertChar);
        }
        self.force_merge = true;
        let mut parts = text.split('\n');
        if let Some(first) = parts.next() {
            self.insert_text(first);
            for rest in parts {
                self.newline();
                self.insert_text(rest);
            }
        }
        self.force_merge = false;
        // whatever follows the paste starts a fresh undo step
        self.last_edit = None;
    }

    pub fn newline(&mut self) {
        let replaced = self.delete_selection();
        if !replaced && !self.mergeable(EditKind::Newline) {
            self.push_undo(EditKind::Newline);
        }
        let (x, y) = self.cursor;
        let line = self.lines[y].clone();
        let byte = char_index_to_byte(&line, x);
        let (left, right) = line.split_at(byte);
        self.lines[y] = left.to_string();
        self.lines.insert(y + 1, right.to_string());
        self.cursor = (0, y + 1);
        self.last_edit = Some((EditKind::Newline, self.cursor));
        self.dirty = true;
        self.mark_edited(y);
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let (x, y) = self.cursor;
        if x == 0 && y == 0 {
            return; // nothing to delete
        }
        if !self.mergeable(EditKind::Backspace) {
            self.push_undo(EditKind::Backspace);
        }
        if x > 0 {
            let line = &mut self.lines[y];
            let byte = char_index_to_byte(line, x - 1);
            line.remove(byte);
            self.cursor.0 -= 1;
        } else {
            let prev_len = self.line_len(y - 1);
            let rest = self.lines.remove(y);
            self.lines[y - 1].push_str(&rest);
            self.cursor = (prev_len, y - 1);
        }
        self.last_edit = Some((EditKind::Backspace, self.cursor));
        self.dirty = true;
        self.mark_edited(self.cursor.1);
    }

    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let (x, y) = self.cursor;
        let line_len = self.line_len(y);
        if x >= line_len && y + 1 >= self.lines.len() {
            return; // nothing to delete
        }
        if !self.mergeable(EditKind::Delete) {
            self.push_undo(EditKind::Delete);
        }
        if x < line_len {
            let line = &mut self.lines[y];
            let byte = char_index_to_byte(line, x);
            line.remove(byte);
        } else {
            let rest = self.lines.remove(y + 1);
            self.lines[y].push_str(&rest);
        }
        self.last_edit = Some((EditKind::Delete, self.cursor));
        self.dirty = true;
        self.mark_edited(y);
    }

    // ---- selection ---------------------------------------------------------

    /// `(start, end)` of the selection, normalized so `start <= end`;
    /// `None` when nothing is selected (or the selection is empty).
    /// Positions are `(char, line)` tuples, but ordering is by line first.
    pub fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.selection_anchor?;
        if anchor == self.cursor {
            return None;
        }
        let before = (anchor.1, anchor.0) <= (self.cursor.1, self.cursor.0);
        Some(if before {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        })
    }

    pub fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    /// Start extending the selection from the current cursor position.
    pub fn begin_selection(&mut self) {
        self.break_chain();
        self.selection_anchor = Some(self.cursor);
        self.selecting = true;
    }

    /// Stop extending the selection (mouse button released), keeping it.
    pub fn end_selection(&mut self) {
        self.break_chain();
        self.selecting = false;
    }

    pub fn clear_selection(&mut self) {
        self.break_chain();
        self.selection_anchor = None;
        self.selecting = false;
    }

    /// Select the whole buffer.
    pub fn select_all(&mut self) {
        self.break_chain();
        let last = self.lines.len() - 1;
        self.selection_anchor = Some((0, 0));
        self.cursor = (self.line_len(last), last);
        self.selecting = false;
    }

    /// The char range of the "word" at `(x, y)` (`x` is a char index), or
    /// `None` when the line is empty. A word is a run of
    /// alphanumeric/underscore chars; whitespace selects its contiguous run;
    /// when the char under the cursor is punctuation, the adjacent run of
    /// punctuation is the "word" instead (so `->` is one). Clicks past the
    /// end of the line look at the last char.
    fn word_range_at(&self, (x, y): (usize, usize)) -> Option<(usize, usize)> {
        let line = self.lines.get(y)?;
        let chars: Vec<char> = line.chars().collect();
        let len = chars.len();
        if len == 0 {
            return None;
        }
        let x = x.min(len);
        let i = if x == len { len - 1 } else { x };
        let word_like = chars[i].is_alphanumeric() || chars[i] == '_';
        let matches = |c: char| {
            if chars[i].is_whitespace() {
                c.is_whitespace()
            } else if word_like {
                c.is_alphanumeric() || c == '_'
            } else {
                !c.is_whitespace() && !c.is_alphanumeric() && c != '_'
            }
        };
        let mut start = i;
        while start > 0 && matches(chars[start - 1]) {
            start -= 1;
        }
        let mut end = i + 1;
        while end < len && matches(chars[end]) {
            end += 1;
        }
        Some((start, end))
    }

    /// Select the "word" at `(x, y)` (`x` is a char index). A word is a
    /// run of alphanumeric/underscore chars; whitespace selects its
    /// contiguous run; when the char under the cursor is punctuation, the
    /// adjacent run of punctuation is selected instead (so double-clicking
    /// `->` selects `->`). An empty line selects nothing.
    pub fn select_word_at(&mut self, (x, y): (usize, usize)) {
        self.break_chain();
        match self.word_range_at((x, y)) {
            Some((start, end)) => {
                self.selection_anchor = Some((start, y));
                self.cursor = (end, y);
                self.selecting = true;
            }
            None => {
                let len = self.lines.get(y).map_or(0, |l| char_count(l));
                self.cursor = (x.min(len), y);
                self.clear_selection();
            }
        }
    }

    /// Extend the selection so its moving end lands at the end of the word
    /// or whitespace run at `(x, y)` (used when dragging after a
    /// double-click).
    pub fn extend_selection_word_at(&mut self, (x, y): (usize, usize)) {
        self.break_chain();
        if self.selection_anchor.is_none() {
            self.begin_selection();
        }
        match self.word_range_at((x, y)) {
            Some((_, end)) => {
                self.cursor = (end, y);
            }
            None => {
                let len = self.lines.get(y).map_or(0, |l| char_count(l));
                self.cursor = (x.min(len), y);
            }
        }
        self.selecting = true;
    }

    /// Extend the selection so its moving end lands at the end of line `y`
    /// (used when dragging after a triple-click).
    pub fn extend_selection_line_at(&mut self, (_x, y): (usize, usize)) {
        if y >= self.lines.len() {
            return;
        }
        self.break_chain();
        if self.selection_anchor.is_none() {
            self.begin_selection();
        }
        self.cursor = (self.line_len(y), y);
        self.selecting = true;
    }

    /// Select the whole line `y` (the line content, not the trailing
    /// newline).
    pub fn select_line(&mut self, y: usize) {
        if y >= self.lines.len() {
            return;
        }
        self.break_chain();
        let len = self.line_len(y);
        self.selection_anchor = Some((0, y));
        self.cursor = (len, y);
        self.selecting = true;
    }

    /// The selected text, if any. Positions are `(char, line)` tuples.
    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        let (start_char, start_line) = start;
        let (end_char, end_line) = end;
        if start_line == end_line {
            let line = &self.lines[start_line];
            let a = char_index_to_byte(line, start_char);
            let b = char_index_to_byte(line, end_char);
            return Some(line[a..b].to_string());
        }
        let mut out = String::new();
        let first = &self.lines[start_line];
        out.push_str(&first[char_index_to_byte(first, start_char)..]);
        for y in start_line + 1..end_line {
            out.push('\n');
            out.push_str(&self.lines[y]);
        }
        out.push('\n');
        let last = &self.lines[end_line];
        out.push_str(&last[..char_index_to_byte(last, end_char)]);
        Some(out)
    }

    /// The selected char range on line `y`, if the selection crosses it.
    pub fn selection_on_line(&self, y: usize) -> Option<(usize, usize)> {
        let (start, end) = self.selection_range()?;
        let (start_char, start_line) = start;
        let (end_char, end_line) = end;
        if y < start_line || y > end_line {
            return None;
        }
        let from = if y == start_line { start_char } else { 0 };
        let to = if y == end_line {
            end_char
        } else {
            self.line_len(y)
        };
        Some((from, to))
    }

    /// Delete the selected text, leaving the cursor at the selection start.
    /// Returns `false` when there was nothing to delete.
    pub fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection_range() else {
            return false;
        };
        if !self.mergeable(EditKind::DeleteSelection) {
            self.push_undo(EditKind::DeleteSelection);
        }
        let (start_char, start_line) = start;
        let (end_char, end_line) = end;
        if start_line == end_line {
            let line = &mut self.lines[start_line];
            let a = char_index_to_byte(line, start_char);
            let b = char_index_to_byte(line, end_char);
            line.replace_range(a..b, "");
        } else {
            // keep the start-line prefix and the end-line suffix, drop the
            // lines in between
            let first = self.lines[start_line].clone();
            let last = self.lines[end_line].clone();
            let prefix = &first[..char_index_to_byte(&first, start_char)];
            let suffix = &last[char_index_to_byte(&last, end_char)..];
            let joined = format!("{prefix}{suffix}");
            self.lines.splice(start_line..=end_line, [joined]);
        }
        self.cursor = start;
        self.clear_selection();
        self.last_edit = Some((EditKind::DeleteSelection, start));
        self.dirty = true;
        self.mark_edited(start_line);
        true
    }

    // ---- undo / redo ------------------------------------------------------

    /// Save the current state as an undo step (dropping the oldest when
    /// the history is full) and discard any redo history.
    fn push_undo(&mut self, kind: EditKind) {
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > MAX_UNDO {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        self.last_edit = Some((kind, self.cursor));
    }

    /// Whether an edit of `kind` starting at the current cursor continues
    /// the previous edit's undo step. Continuous typing, backspacing and
    /// deleting merge; anything else (different kind, cursor movement or
    /// a selection change) starts a new step.
    fn mergeable(&self, kind: EditKind) -> bool {
        if self.force_merge {
            return true;
        }
        let Some((last_kind, last_pos)) = self.last_edit else {
            return false;
        };
        match (last_kind, kind) {
            (EditKind::InsertChar, EditKind::InsertChar)
            | (EditKind::Backspace, EditKind::Backspace)
            | (EditKind::Delete, EditKind::Delete)
            | (EditKind::Newline, EditKind::Newline) => self.cursor == last_pos,
            _ => false,
        }
    }

    /// Break the merge chain: the next edit starts a fresh undo step.
    /// Called after cursor movement, selection changes and saves.
    fn break_chain(&mut self) {
        self.last_edit = None;
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            cursor: self.cursor,
            selection_anchor: self.selection_anchor,
            selecting: self.selecting,
            dirty: self.dirty,
        }
    }

    fn restore(&mut self, snap: Snapshot) {
        self.lines = snap.lines;
        self.cursor = snap.cursor;
        self.selection_anchor = snap.selection_anchor;
        self.selecting = snap.selecting;
        self.dirty = snap.dirty;
    }

    /// Undo the last edit. Returns `true` when something was undone and
    /// sets `last_edit_line` to the first changed line (for
    /// re-highlighting). The restored state also brings back the cursor,
    /// selection and dirty flag.
    pub fn undo(&mut self) -> bool {
        let Some(snap) = self.undo_stack.pop() else {
            return false;
        };
        let first_diff = first_diff_line(&snap.lines, &self.lines);
        self.redo_stack.push(self.snapshot());
        self.restore(snap);
        // the dirty flag cannot be snapshotted across a save: recompute it
        // against the content the file was last saved with
        self.dirty = self.clean_lines != self.lines;
        self.last_edit = None;
        self.force_merge = false;
        self.last_edit_line = first_diff;
        true
    }

    /// Redo the last undone edit. Returns `true` when something was
    /// redone and sets `last_edit_line` to the first changed line.
    pub fn redo(&mut self) -> bool {
        let Some(snap) = self.redo_stack.pop() else {
            return false;
        };
        let first_diff = first_diff_line(&snap.lines, &self.lines);
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > MAX_UNDO {
            self.undo_stack.remove(0);
        }
        self.restore(snap);
        self.dirty = self.clean_lines != self.lines;
        self.last_edit = None;
        self.force_merge = false;
        self.last_edit_line = first_diff;
        true
    }

    // ---- cursor movement ---------------------------------------------------

    pub fn move_left(&mut self) {
        self.break_chain();
        if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
        } else if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
            self.cursor.0 = self.line_len(self.cursor.1);
        }
    }

    pub fn move_right(&mut self) {
        self.break_chain();
        let len = self.line_len(self.cursor.1);
        if self.cursor.0 < len {
            self.cursor.0 += 1;
        } else if self.cursor.1 + 1 < self.lines.len() {
            self.cursor.1 += 1;
            self.cursor.0 = 0;
        }
    }

    pub fn move_up(&mut self) {
        self.break_chain();
        if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
            self.clamp_x();
        }
    }

    pub fn move_down(&mut self) {
        self.break_chain();
        if self.cursor.1 + 1 < self.lines.len() {
            self.cursor.1 += 1;
            self.clamp_x();
        }
    }

    pub fn home(&mut self) {
        self.break_chain();
        self.cursor.0 = 0;
    }

    pub fn end(&mut self) {
        self.break_chain();
        self.cursor.0 = self.line_len(self.cursor.1);
    }

    pub fn page_up(&mut self, rows: usize) {
        self.break_chain();
        if rows > 0 {
            self.cursor.1 = self.cursor.1.saturating_sub(rows);
            self.clamp_x();
        }
    }

    pub fn page_down(&mut self, rows: usize) {
        self.break_chain();
        if rows > 0 {
            let last = self.lines.len() - 1;
            self.cursor.1 = (self.cursor.1 + rows).min(last);
            self.clamp_x();
        }
    }

    // ---- scrolling / rendering --------------------------------------------

    /// Scroll so the cursor is inside the visible viewport
    /// (`view_w` x `view_h` chars).
    pub fn ensure_visible(&mut self, view_h: usize, view_w: usize) {
        // guard against an out-of-range cursor (shouldn't happen through
        // normal movement, which clamps)
        self.clamp_x();
        if self.cursor.1 >= self.lines.len() {
            self.cursor.1 = self.lines.len() - 1;
        }
        if self.cursor.1 < self.scroll.1 {
            self.scroll.1 = self.cursor.1;
        }
        if self.cursor.1 >= self.scroll.1 + view_h {
            self.scroll.1 = self.cursor.1.saturating_add(1).saturating_sub(view_h);
        }
        if self.cursor.0 < self.scroll.0 {
            self.scroll.0 = self.cursor.0;
        }
        if self.cursor.0 >= self.scroll.0 + view_w {
            self.scroll.0 = self.cursor.0.saturating_add(1).saturating_sub(view_w);
        }
    }

    /// Terminal column of the cursor relative to the visible slice,
    /// accounting for wide (CJK etc.) characters.
    pub fn cursor_col(&self) -> usize {
        let line = &self.lines[self.cursor.1];
        let to_cursor: usize = line
            .chars()
            .take(self.cursor.0)
            .map(|c| c.width().unwrap_or(0))
            .sum();
        let to_scroll: usize = line
            .chars()
            .take(self.scroll.0)
            .map(|c| c.width().unwrap_or(0))
            .sum();
        to_cursor.saturating_sub(to_scroll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> Buffer {
        Buffer::empty()
    }

    #[test]
    fn empty_buffer_has_one_line() {
        let b = empty();
        assert_eq!(b.lines, vec![""]);
        assert!(!b.dirty);
    }

    #[test]
    fn typing_then_backspacing_round_trips() {
        let mut b = empty();
        b.insert_text("hello");
        assert_eq!(b.lines, vec!["hello"]);
        assert_eq!(b.cursor, (5, 0));
        assert!(b.dirty);
        for _ in 0..5 {
            b.backspace();
        }
        assert_eq!(b.lines, vec![""]);
        assert_eq!(b.cursor, (0, 0));
    }

    #[test]
    fn newline_splits_line_at_cursor() {
        let mut b = empty();
        b.insert_text("abcd");
        b.move_left();
        b.move_left();
        b.newline();
        assert_eq!(b.lines, vec!["ab", "cd"]);
        assert_eq!(b.cursor, (0, 1));
    }

    #[test]
    fn enter_at_end_creates_empty_line() {
        let mut b = empty();
        b.insert_text("ab");
        b.newline();
        assert_eq!(b.lines, vec!["ab", ""]);
        assert_eq!(b.cursor, (0, 1));
    }

    #[test]
    fn backspace_merges_lines() {
        let mut b = empty();
        b.insert_text("one");
        b.newline();
        b.insert_text("two");
        b.move_left();
        b.move_left();
        b.move_left();
        b.backspace();
        assert_eq!(b.lines, vec!["onetwo"]);
        assert_eq!(b.cursor, (3, 0));
    }

    #[test]
    fn delete_merges_lines_at_end() {
        let mut b = empty();
        b.insert_text("one");
        b.newline();
        b.insert_text("two");
        b.move_up();
        b.end();
        b.delete();
        assert_eq!(b.lines, vec!["onetwo"]);
        assert_eq!(b.cursor, (3, 0));
    }

    #[test]
    fn backspace_at_very_start_does_nothing() {
        let mut b = empty();
        b.backspace();
        assert_eq!(b.lines, vec![""]);
        assert_eq!(b.cursor, (0, 0));
    }

    #[test]
    fn left_at_line_start_jumps_to_previous_line_end() {
        let mut b = empty();
        b.insert_text("ab");
        b.newline();
        b.move_left();
        assert_eq!(b.cursor, (2, 0));
    }

    #[test]
    fn right_at_line_end_jumps_to_next_line() {
        let mut b = empty();
        b.insert_text("ab");
        b.newline();
        b.move_up();
        b.move_right();
        b.move_right();
        b.move_right();
        assert_eq!(b.cursor, (0, 1));
    }

    #[test]
    fn unicode_insert_and_backspace_by_char() {
        let mut b = empty();
        b.insert_text("aé日");
        assert_eq!(b.lines, vec!["aé日"]);
        assert_eq!(b.cursor, (3, 0));
        b.backspace();
        assert_eq!(b.lines, vec!["aé"]);
        // insert in the middle of a multibyte line
        b.home();
        b.move_right();
        b.insert_char('x');
        assert_eq!(b.lines, vec!["axé"]);
    }

    #[test]
    fn wide_char_cursor_column() {
        let mut b = empty();
        b.insert_text("日本語");
        b.end();
        // 3 wide chars -> column 6
        assert_eq!(b.cursor_col(), 6);
        // scroll so the first char is off-screen
        b.scroll.0 = 2;
        assert_eq!(b.cursor_col(), 2);
    }

    #[test]
    fn ensure_visible_scrolls_down_and_right() {
        let mut b = empty();
        // 20 lines total (0..=19); cursor ends on line 19
        for _ in 0..19 {
            b.newline();
            b.insert_text("x");
        }
        // make the last line long enough that x = 50 is valid
        b.end();
        for _ in 0..49 {
            b.insert_char('y');
        }
        b.cursor = (50, 19);
        b.ensure_visible(10, 30);
        assert_eq!(b.scroll.1, 10);
        assert_eq!(b.scroll.0, 21);
        // and back up
        b.cursor = (1, 3);
        b.ensure_visible(10, 30);
        assert_eq!(b.scroll.1, 3);
        assert_eq!(b.scroll.0, 1);
    }

    #[test]
    fn page_up_down_clamp() {
        let mut b = empty();
        for _ in 0..10 {
            b.newline();
        }
        // 11 lines: 0..=10
        b.cursor = (0, 10);
        b.page_down(5);
        assert_eq!(b.cursor.1, 10);
        b.page_up(5);
        assert_eq!(b.cursor.1, 5);
        b.page_up(100);
        assert_eq!(b.cursor.1, 0);
    }

    // ---- save / load round trips ------------------------------------------

    /// Scratch area inside the workspace (the sandbox blocks /tmp writes).
    fn tmp_path(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn save_load_round_trip_with_trailing_newline() {
        let path = tmp_path("with-nl.txt");
        std::fs::write(&path, "one\ntwo\n").unwrap();

        let loaded = Buffer::from_path(path.clone()).unwrap();
        assert_eq!(loaded.lines, vec!["one", "two", ""]);
        // saving the loaded buffer reproduces the file exactly
        let mut loaded = loaded;
        loaded.save().unwrap();
        assert!(!loaded.dirty);
        let disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(disk, "one\ntwo\n");

        // deleting the trailing empty line removes the trailing newline
        loaded.home();
        loaded.move_down();
        loaded.move_down();
        loaded.backspace();
        loaded.save().unwrap();
        let disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(disk, "one\ntwo");
    }

    #[test]
    fn save_load_round_trip_without_trailing_newline() {
        let path = tmp_path("no-nl.txt");
        let mut b = empty();
        b.insert_text("one");
        b.newline();
        b.insert_text("two");
        b.path = Some(path.clone());
        b.save().unwrap();
        let disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(disk, "one\ntwo");

        let loaded = Buffer::from_path(path).unwrap();
        assert_eq!(loaded.lines, vec!["one", "two"]);
    }

    #[test]
    fn empty_file_loads_as_single_empty_line() {
        let path = tmp_path("empty.txt");
        std::fs::write(&path, "").unwrap();
        let loaded = Buffer::from_path(path).unwrap();
        assert_eq!(loaded.lines, vec![""]);
    }

    // ---- selection ---------------------------------------------------------

    fn typed(b: &mut Buffer, text: &str) {
        b.insert_text(text);
    }

    #[test]
    fn selection_requires_anchor_different_from_cursor() {
        let mut b = empty();
        typed(&mut b, "ab");
        b.home();
        assert!(!b.has_selection());
        b.begin_selection();
        assert!(!b.has_selection()); // anchor == cursor
        b.move_right();
        assert!(b.has_selection());
        assert_eq!(b.selection_range(), Some(((0, 0), (1, 0))));
        // moving backwards normalizes the range
        b.begin_selection();
        b.home();
        assert_eq!(b.selection_range(), Some(((0, 0), (1, 0))));
    }

    #[test]
    fn selected_text_same_line_and_multi_line() {
        let mut b = empty();
        typed(&mut b, "one two");
        b.home();
        b.begin_selection();
        b.move_right();
        b.move_right();
        b.move_right();
        assert_eq!(b.selected_text().as_deref(), Some("one"));

        // multi-line selection: (0,4) .. (2,5)
        let mut b = empty();
        typed(&mut b, "one two");
        b.newline();
        typed(&mut b, "second");
        b.newline();
        typed(&mut b, "third");
        b.home();
        b.move_up();
        b.move_up();
        b.move_right();
        b.move_right();
        b.move_right();
        b.move_right();
        b.begin_selection();
        b.end();
        b.move_down();
        b.move_down();
        b.move_right();
        b.move_right();
        b.move_right();
        assert_eq!(b.selected_text().as_deref(), Some("two\nsecond\nthird"));
    }

    #[test]
    fn selection_on_line_ranges() {
        let mut b = empty();
        typed(&mut b, "abc");
        b.newline();
        typed(&mut b, "def");
        b.newline();
        typed(&mut b, "ghi");
        b.home();
        b.move_up();
        b.move_up();
        b.move_down();
        b.move_right();
        b.begin_selection(); // anchor (1,1)
        b.move_down();
        b.move_right();
        b.move_right();
        assert_eq!(b.selection_on_line(0), None);
        assert_eq!(b.selection_on_line(1), Some((1, 3)));
        assert_eq!(b.selection_on_line(2), Some((0, 3)));
    }

    #[test]
    fn delete_selection_same_line() {
        let mut b = empty();
        typed(&mut b, "hello world");
        b.home();
        b.move_right();
        b.move_right();
        b.begin_selection();
        b.move_right();
        b.move_right();
        b.move_right();
        b.move_right();
        b.move_right();
        b.delete_selection();
        assert_eq!(b.lines, vec!["heorld"]); // "llo w" was removed
        assert_eq!(b.cursor, (2, 0));
        assert!(!b.has_selection());
    }

    #[test]
    fn delete_selection_multi_line() {
        let mut b = empty();
        typed(&mut b, "abc");
        b.newline();
        typed(&mut b, "def");
        b.newline();
        typed(&mut b, "ghi");
        // select from (1,0) to (3,2): "bc" + "def" + "ghi"
        b.home();
        b.move_up();
        b.move_up();
        b.move_right();
        b.begin_selection();
        b.move_down();
        b.move_down();
        b.move_right();
        b.move_right();
        b.delete_selection();
        assert_eq!(b.lines, vec!["a"]);
        assert_eq!(b.cursor, (1, 0));
    }

    #[test]
    fn typing_replaces_selection() {
        let mut b = empty();
        typed(&mut b, "hello");
        b.home();
        b.move_right();
        b.begin_selection();
        b.move_right();
        b.move_right();
        b.insert_char('X');
        assert_eq!(b.lines, vec!["hXlo"]); // "el" was replaced
        assert!(!b.has_selection());
    }

    #[test]
    fn backspace_and_delete_replace_selection() {
        let mut b = empty();
        typed(&mut b, "hello");
        b.home();
        b.begin_selection();
        b.end();
        b.backspace();
        assert_eq!(b.lines, vec![""]);

        typed(&mut b, "hello");
        b.home();
        b.begin_selection();
        b.end();
        b.delete();
        assert_eq!(b.lines, vec![""]);
    }

    #[test]
    fn select_all_and_selected_text() {
        let mut b = empty();
        typed(&mut b, "one");
        b.newline();
        typed(&mut b, "two");
        b.select_all();
        assert_eq!(b.selected_text().as_deref(), Some("one\ntwo"));
        // select all + type replaces everything
        b.insert_char('z');
        assert_eq!(b.lines, vec!["z"]);
    }

    #[test]
    fn select_word_at_selects_alphanumeric_run() {
        let mut b = empty();
        typed(&mut b, "foo_bar baz");
        b.select_word_at((4, 0)); // inside foo_bar
        assert_eq!(b.selected_text().as_deref(), Some("foo_bar"));
        // the next word
        b.select_word_at((8, 0)); // inside baz
        assert_eq!(b.selected_text().as_deref(), Some("baz"));
        // clicking past the end of the line selects the last word
        b.select_word_at((11, 0));
        assert_eq!(b.selected_text().as_deref(), Some("baz"));
    }

    #[test]
    fn select_word_at_selects_punctuation_run() {
        let mut b = empty();
        typed(&mut b, "a -> b");
        b.select_word_at((3, 0)); // on '-'
        assert_eq!(b.selected_text().as_deref(), Some("->"));
        // punctuation runs stop at word chars: clicking '.' in a.b.c
        let mut b = empty();
        typed(&mut b, "a.b.c");
        b.select_word_at((1, 0));
        assert_eq!(b.selected_text().as_deref(), Some("."));
    }

    #[test]
    fn select_word_at_selects_whitespace() {
        let mut b = empty();
        typed(&mut b, "hello   world");
        b.select_word_at((1, 0));
        assert_eq!(b.selected_text().as_deref(), Some("hello"));
        // clicking the whitespace between words selects its whole run
        b.select_word_at((6, 0));
        assert_eq!(b.selected_text().as_deref(), Some("   "));
        assert_eq!(b.selection_range(), Some(((5, 0), (8, 0))));
        // an empty line selects nothing
        let mut b = empty();
        b.newline();
        b.select_word_at((0, 1));
        assert!(!b.has_selection());
    }

    #[test]
    fn select_word_at_handles_unicode() {
        let mut b = empty();
        typed(&mut b, "héllo wörld");
        b.select_word_at((3, 0)); // inside héllo
        assert_eq!(b.selected_text().as_deref(), Some("héllo"));
        b.select_word_at((7, 0)); // inside wörld
        assert_eq!(b.selected_text().as_deref(), Some("wörld"));
    }

    #[test]
    fn select_line_selects_whole_line() {
        let mut b = empty();
        typed(&mut b, "one two");
        b.newline();
        typed(&mut b, "three");
        b.select_line(1);
        assert_eq!(b.selected_text().as_deref(), Some("three"));
        assert_eq!(b.selection_range(), Some(((0, 1), (5, 1))));
        // an empty line still yields an empty (non-)selection
        b.newline();
        b.select_line(2);
        assert!(!b.has_selection());
        assert_eq!(b.cursor, (0, 2));
    }

    #[test]
    fn extend_selection_word_at_moves_cursor_to_word_end() {
        let mut b = empty();
        typed(&mut b, "one two three");
        b.select_word_at((0, 0)); // "one"
        // drag into "three" -> the selection ends at its word end
        b.extend_selection_word_at((8, 0));
        assert_eq!(b.selected_text().as_deref(), Some("one two three"));
        // dragging back into the first word shrinks it again
        b.extend_selection_word_at((2, 0));
        assert_eq!(b.selected_text().as_deref(), Some("one"));
        // dragging onto whitespace extends to the end of its run
        b.extend_selection_word_at((3, 0));
        assert_eq!(b.selected_text().as_deref(), Some("one "));
        assert_eq!(b.cursor, (4, 0));
    }

    #[test]
    fn extend_selection_line_at_moves_cursor_to_line_end() {
        let mut b = empty();
        typed(&mut b, "one");
        b.newline();
        typed(&mut b, "two");
        b.newline();
        typed(&mut b, "three");
        b.select_line(0);
        b.extend_selection_line_at((0, 2));
        assert_eq!(b.selected_text().as_deref(), Some("one\ntwo\nthree"));
    }

    #[test]
    fn insert_multiline_with_newlines() {
        let mut b = empty();
        typed(&mut b, "ab");
        b.newline();
        typed(&mut b, "cd");
        b.home();
        b.move_down();
        b.insert_multiline("X\nY\nZ");
        assert_eq!(b.lines, vec!["ab", "X", "Y", "Zcd"]);
        // select from line 2 through the end of line 3 and replace with "Q"
        b.home();
        b.move_up();
        b.begin_selection();
        b.move_down();
        b.end();
        b.insert_multiline("Q");
        assert_eq!(b.lines, vec!["ab", "X", "Q"]);
    }

    #[test]
    fn multi_line_edit_tracks_first_changed_line() {
        let mut b = empty();
        typed(&mut b, "abc");
        b.newline();
        typed(&mut b, "def");
        b.home();
        b.insert_multiline("1\n2\n3");
        assert_eq!(b.last_edit_line, Some(0));
    }

    #[test]
    fn crlf_files_are_not_corrupted() {
        let path = tmp_path("crlf.txt");
        std::fs::write(&path, "a\r\nb\r\n").unwrap();
        let loaded = Buffer::from_path(path.clone()).unwrap();
        assert_eq!(loaded.lines, vec!["a\r", "b\r", ""]);
        let mut loaded = loaded;
        loaded.save().unwrap();
        let disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(disk, "a\r\nb\r\n");
    }

    #[test]
    fn save_without_path_errors() {
        let mut b = empty();
        assert!(b.save().is_err());
    }

    // ---- undo / redo ------------------------------------------------------

    #[test]
    fn undo_redo_round_trip_restores_text_cursor_and_dirty() {
        let mut b = empty();
        b.insert_text("hello");
        assert!(b.dirty);
        b.undo();
        assert_eq!(b.lines, vec![""]);
        assert_eq!(b.cursor, (0, 0));
        assert!(!b.dirty); // back to the pristine state
        b.redo();
        assert_eq!(b.lines, vec!["hello"]);
        assert_eq!(b.cursor, (5, 0));
        assert!(b.dirty);
        b.undo();
        assert_eq!(b.lines, vec![""]);
        assert!(!b.dirty);
    }

    #[test]
    fn continuous_typing_is_one_undo_step() {
        let mut b = empty();
        b.insert_text("hello");
        b.undo();
        assert_eq!(b.lines, vec![""]);
        b.redo();
        assert_eq!(b.lines, vec!["hello"]);
    }

    #[test]
    fn cursor_movement_splits_typing_runs() {
        let mut b = empty();
        b.insert_text("ab");
        b.move_left();
        b.insert_char('X');
        assert_eq!(b.lines, vec!["aXb"]);
        // three separate undo steps: "a", "b", then "X"
        b.undo();
        assert_eq!(b.lines, vec!["ab"]);
        assert_eq!(b.cursor, (1, 0)); // where the undone edit started
        b.undo();
        assert_eq!(b.lines, vec![""]);
        b.redo();
        b.redo();
        assert_eq!(b.lines, vec!["aXb"]);
    }

    #[test]
    fn backspace_run_is_one_undo_step() {
        let mut b = empty();
        b.insert_text("hello");
        b.backspace();
        b.backspace();
        assert_eq!(b.lines, vec!["hel"]);
        b.undo();
        assert_eq!(b.lines, vec!["hello"]);
        assert_eq!(b.cursor, (5, 0));
        b.undo();
        assert_eq!(b.lines, vec![""]);
    }

    #[test]
    fn delete_run_is_one_undo_step() {
        let mut b = empty();
        b.insert_text("abc");
        b.home();
        b.delete();
        b.delete();
        assert_eq!(b.lines, vec!["c"]);
        b.undo();
        assert_eq!(b.lines, vec!["abc"]);
        assert_eq!(b.cursor, (0, 0));
    }

    #[test]
    fn consecutive_newlines_merge_but_typing_after_enter_does_not() {
        let mut b = empty();
        b.insert_text("ab");
        b.newline();
        b.insert_text("cd");
        // entries: [before "ab"], [before the newline], [before "cd"]
        b.undo();
        assert_eq!(b.lines, vec!["ab", ""]);
        b.undo();
        assert_eq!(b.lines, vec!["ab"]);
        b.undo();
        assert_eq!(b.lines, vec![""]);
        b.redo();
        b.redo();
        b.redo();
        assert_eq!(b.lines, vec!["ab", "cd"]);

        // but consecutive enters are a single step
        let mut b = empty();
        b.newline();
        b.newline();
        b.undo();
        assert_eq!(b.lines, vec![""]);
    }

    #[test]
    fn paste_is_single_undo_step() {
        let mut b = empty();
        b.insert_text("ab");
        b.newline();
        b.insert_text("cd");
        b.home();
        b.move_down();
        b.insert_multiline("X\nY\nZ");
        assert_eq!(b.lines, vec!["ab", "X", "Y", "Zcd"]);
        b.undo();
        assert_eq!(b.lines, vec!["ab", "cd"]);
        assert_eq!(b.cursor, (0, 1));
    }

    #[test]
    fn typing_over_selection_is_single_undo_step() {
        let mut b = empty();
        b.insert_text("hello");
        b.home();
        b.begin_selection();
        b.move_right();
        b.move_right();
        b.insert_char('X');
        assert_eq!(b.lines, vec!["Xllo"]);
        b.undo();
        assert_eq!(b.lines, vec!["hello"]);
        // the replaced selection comes back along with the text
        assert_eq!(b.selection_anchor, Some((0, 0)));
        assert_eq!(b.cursor, (2, 0));
    }

    #[test]
    fn paste_over_selection_is_single_undo_step() {
        let mut b = empty();
        b.insert_text("abc");
        b.home();
        b.begin_selection();
        b.move_right();
        b.insert_multiline("XY\nZ");
        assert_eq!(b.lines, vec!["XY", "Zbc"]);
        b.undo();
        assert_eq!(b.lines, vec!["abc"]);
        assert_eq!(b.selection_anchor, Some((0, 0)));
        assert_eq!(b.cursor, (1, 0));
    }

    #[test]
    fn cut_undo_restores_selection() {
        let mut b = empty();
        b.insert_text("hello");
        b.home();
        b.begin_selection();
        b.end();
        b.delete_selection();
        assert_eq!(b.lines, vec![""]);
        assert!(!b.has_selection());
        b.undo();
        assert_eq!(b.lines, vec!["hello"]);
        assert_eq!(b.selection_anchor, Some((0, 0)));
        assert_eq!(b.cursor, (5, 0));
    }

    #[test]
    fn new_edit_after_undo_clears_redo() {
        let mut b = empty();
        b.insert_text("a");
        b.undo();
        b.insert_text("b");
        assert!(!b.redo());
        assert_eq!(b.lines, vec!["b"]);
        b.undo();
        assert_eq!(b.lines, vec![""]);
    }

    #[test]
    fn undo_and_redo_are_noops_on_empty_history() {
        let mut b = empty();
        assert!(!b.undo());
        assert!(!b.redo());
        b.insert_text("x");
        b.undo();
        assert!(!b.undo()); // already at the beginning
        b.redo();
        assert!(!b.redo()); // already at the end
    }

    #[test]
    fn undo_history_is_capped() {
        let mut b = empty();
        for _ in 0..MAX_UNDO + 50 {
            b.move_right(); // break the typing chain each time
            b.insert_char('x');
        }
        assert_eq!(b.undo_stack.len(), MAX_UNDO);
        // undoing everything stops at the oldest kept snapshot: the
        // dropped entries' edits stay applied
        while b.undo() {}
        assert_eq!(b.lines, vec!["x".repeat(50)]);
    }

    #[test]
    fn save_breaks_the_typing_chain() {
        let path = tmp_path("undo-save.txt");
        let mut b = empty();
        b.path = Some(path.clone());
        b.insert_text("ab");
        b.save().unwrap();
        b.insert_text("c");
        // undoing the post-save typing stops at the saved text
        b.undo();
        assert_eq!(b.lines, vec!["ab"]);
        assert!(!b.dirty);
        b.undo();
        assert_eq!(b.lines, vec![""]);
    }

    #[test]
    fn undo_sets_last_edit_line_for_rehighlighting() {
        let mut b = empty();
        b.insert_text("abc");
        b.newline();
        b.insert_text("def");
        b.home();
        b.move_down();
        b.insert_char('X'); // line 1 changed
        b.undo();
        assert_eq!(b.last_edit_line, Some(1));
        b.redo();
        assert_eq!(b.last_edit_line, Some(1));
    }
}
