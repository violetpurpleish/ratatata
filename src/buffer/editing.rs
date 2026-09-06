//! Text insertion, replacement, deletion, indentation, and paste grouping.

use super::*;

impl Buffer {
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
        // insert_char/newline calls into it via force_merge. When already
        // merging (a compound replace-all), don't start a nested undo step
        // and don't clear the outer merge flag.
        let already_merging = self.force_merge;
        if !already_merging {
            if self.has_selection() {
                self.push_undo(EditKind::DeleteSelection);
            } else {
                self.push_undo(EditKind::InsertChar);
            }
            self.force_merge = true;
        }
        let normalized = text.replace("\r\n", "\n");
        let mut parts = normalized.split('\n');
        if let Some(first) = parts.next() {
            self.insert_text(first);
            for rest in parts {
                self.newline_inner(false);
                self.insert_text(rest);
            }
        }
        if !already_merging {
            self.force_merge = false;
            // whatever follows the paste starts a fresh undo step
            self.last_edit = None;
        }
    }

    /// Replace a single-line char range with `text` (which may contain
    /// newlines). One undo step.
    pub fn replace_line_range(&mut self, line: usize, start: usize, end: usize, text: &str) {
        self.selection_anchor = Some((start, line));
        self.cursor = (end, line);
        self.selecting = false;
        if text.is_empty() {
            let _ = self.delete_selection();
        } else {
            self.insert_multiline(text);
        }
    }

    /// Replace several single-line char ranges with the same `text`, last
    /// to first so later indices stay valid. One undo step for the set.
    pub fn replace_line_ranges(&mut self, ranges: &[(usize, usize, usize)], text: &str) {
        if ranges.is_empty() {
            return;
        }
        self.push_undo(EditKind::DeleteSelection);
        self.force_merge = true;
        for &(line, start, end) in ranges.iter().rev() {
            self.selection_anchor = Some((start, line));
            self.cursor = (end, line);
            self.selecting = false;
            if text.is_empty() {
                let _ = self.delete_selection();
            } else {
                self.insert_multiline(text);
            }
        }
        self.force_merge = false;
        self.last_edit = None;
    }

    /// Insert a line break at the cursor. The new line inherits the
    /// leading whitespace of the line being split, so pressing Enter in an
    /// indented block continues the indentation.
    pub fn newline(&mut self) {
        self.newline_inner(true);
    }

    /// Insert a line break; `auto_indent` controls whether the new line
    /// inherits the current line's leading whitespace. Pastes pass `false`
    /// so clipboard content stays verbatim.
    pub(super) fn newline_inner(&mut self, auto_indent: bool) {
        let replaced = self.delete_selection();
        if !replaced && !self.mergeable(EditKind::Newline) {
            self.push_undo(EditKind::Newline);
        }
        let (x, y) = self.cursor;
        let line = self.lines[y].clone();
        let byte = char_index_to_byte(&line, x);
        let (left, right) = line.split_at(byte);
        self.lines[y] = left.to_string();
        let indent = if auto_indent {
            leading_whitespace(&line).to_string()
        } else {
            String::new()
        };
        let mut rest = indent.clone();
        rest.push_str(right);
        self.lines.insert(y + 1, rest);
        self.cursor = (indent.chars().count(), y + 1);
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

    /// Indent (Tab): with a selection, indent every line it touches and
    /// keep the selection; otherwise insert an indent unit at the cursor.
    pub fn indent(&mut self) {
        if self.indent_selection() {
            return;
        }
        self.insert_text(INDENT_UNIT);
    }

    /// Dedent (Shift+Tab): with a selection, remove one indent unit from
    /// every line it touches; otherwise remove one indent unit from the
    /// current line. Does nothing when there is no indentation to remove.
    pub fn dedent(&mut self) {
        if self.dedent_selection() {
            return;
        }
        let (x, y) = self.cursor;
        let n = count_indent_unit(&self.lines[y]);
        if n == 0 {
            return;
        }
        if !self.mergeable(EditKind::Dedent) {
            self.push_undo(EditKind::Dedent);
        }
        remove_indent_unit(&mut self.lines[y]);
        self.cursor.0 = x.saturating_sub(n);
        self.last_edit = Some((EditKind::Dedent, self.cursor));
        self.dirty = true;
        self.mark_edited(y);
    }

    /// Indent every line the selection touches, keeping the selection.
    /// Returns false when there is no selection.
    pub(super) fn indent_selection(&mut self) -> bool {
        let Some(((_, ay), (_, by))) = self.selection_range() else {
            return false;
        };
        if !self.mergeable(EditKind::Indent) {
            self.push_undo(EditKind::Indent);
        }
        for y in ay..=by {
            self.lines[y].insert_str(0, INDENT_UNIT);
        }
        // positions at the very start of a line stay pinned to the start;
        // every other position shifts right with its line
        let shift = |pos: &mut (usize, usize)| {
            if pos.1 >= ay && pos.1 <= by && pos.0 > 0 {
                pos.0 += INDENT_UNIT.len();
            }
        };
        shift(&mut self.cursor);
        shift(self.selection_anchor.as_mut().expect("selection"));
        self.last_edit = Some((EditKind::Indent, self.cursor));
        self.dirty = true;
        self.mark_edited(ay);
        true
    }

    /// Dedent every line the selection touches, keeping the selection.
    /// Returns false when there is no selection (or nothing to dedent).
    pub(super) fn dedent_selection(&mut self) -> bool {
        let Some(((_, ay), (_, by))) = self.selection_range() else {
            return false;
        };
        // count first: if no line is indented, leave everything untouched
        let removed: Vec<usize> = (ay..=by)
            .map(|y| count_indent_unit(&self.lines[y]))
            .collect();
        if removed.iter().all(|&n| n == 0) {
            return false;
        }
        if !self.mergeable(EditKind::Dedent) {
            self.push_undo(EditKind::Dedent);
        }
        for y in ay..=by {
            remove_indent_unit(&mut self.lines[y]);
        }
        let unshift = |pos: &mut (usize, usize)| {
            if pos.1 >= ay && pos.1 <= by {
                pos.0 = pos.0.saturating_sub(removed[pos.1 - ay]);
            }
        };
        unshift(&mut self.cursor);
        unshift(self.selection_anchor.as_mut().expect("selection"));
        self.last_edit = Some((EditKind::Dedent, self.cursor));
        self.dirty = true;
        self.mark_edited(ay);
        true
    }
}
