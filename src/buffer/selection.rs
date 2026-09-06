//! Selection ranges, word/line gestures, extraction, and deletion.

use super::*;

impl Buffer {
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
        self.sync_parinfer_cursor();
    }

    /// The char range of the "word" at `(x, y)` (`x` is a char index), or
    /// `None` when the line is empty. A word is a run of
    /// alphanumeric/underscore chars; whitespace selects its contiguous run;
    /// when the char under the cursor is punctuation, the adjacent run of
    /// punctuation is the "word" instead (so `->` is one). Clicks past the
    /// end of the line look at the last char.
    pub(super) fn word_range_at(&self, (x, y): (usize, usize)) -> Option<(usize, usize)> {
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
}
