//! Logical and visual-row cursor movement.

use super::*;

impl Buffer {
    // ---- cursor movement ---------------------------------------------------

    /// Move the cursor up one visual row. With wrapping, moving up from a
    /// continuation row stays on the same logical line (the previous row);
    /// at the top of a wrapped line it moves to the previous line's last
    /// row. `x` is clamped into the target row.
    pub fn move_up(&mut self) {
        self.break_chain();
        if self.wrap {
            let width = self.wrap_width.max(1);
            let (x, y) = self.cursor;
            let row = visual_row_of(&self.lines[y], x, width);
            if row > 0 {
                let (start, end) = visual_chunk(&self.lines[y], row - 1, width);
                // `end` is exclusive, and a cursor at `end` belongs to the
                // following visual row (`visual_row_of` uses `x < end`).
                // Clamping to `end` therefore leaves Up at the start of the
                // current row and repeated Up appears to stop. Keep the
                // target inside the previous row instead.
                let target = if start < end {
                    x.clamp(start, end - 1)
                } else {
                    start
                };
                let nx = snap_visible_back(&self.lines[y], target, width);
                self.cursor = (nx, y);
            } else if y > 0 {
                self.cursor.1 -= 1;
                self.clamp_x();
            }
            return;
        }
        if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
            self.clamp_x();
        }
    }

    /// Move the cursor down one visual row. With wrapping, moving down
    /// from the last row of a wrapped line moves to the next logical
    /// line; otherwise it stays on the same line (the next row). `x` is
    /// clamped into the target row.
    pub fn move_down(&mut self) {
        self.break_chain();
        if self.wrap {
            let width = self.wrap_width.max(1);
            let (x, y) = self.cursor;
            let row = visual_row_of(&self.lines[y], x, width);
            if row + 1 < visual_len(&self.lines[y], width) {
                let (start, end) = visual_chunk(&self.lines[y], row + 1, width);
                // As above, do not put the cursor at the exclusive end of a
                // visual row: that position is rendered on the next row.
                let target = if start < end {
                    x.clamp(start, end - 1)
                } else {
                    start
                };
                let nx = snap_visible_back(&self.lines[y], target, width);
                self.cursor = (nx, y);
            } else if y + 1 < self.lines.len() {
                self.cursor.1 += 1;
                self.clamp_x();
            }
            return;
        }
        if self.cursor.1 + 1 < self.lines.len() {
            self.cursor.1 += 1;
            self.clamp_x();
        }
    }

    pub fn move_left(&mut self) {
        self.break_chain();
        // Character movement is already visual-row-correct: the caret sits
        // on a char, and the char before the first char of a wrapped row is
        // the last char of the previous row. With word wrap, positions that
        // land inside elided whitespace snap to the previous visible char.
        if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
        } else if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
            self.cursor.0 = self.line_len(self.cursor.1);
        }
        if self.wrap {
            let width = self.wrap_width.max(1);
            self.cursor.0 = snap_visible_back(&self.lines[self.cursor.1], self.cursor.0, width);
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
        if self.wrap {
            let width = self.wrap_width.max(1);
            self.cursor.0 = snap_visible_fwd(&self.lines[self.cursor.1], self.cursor.0, width);
        }
    }

    pub fn home(&mut self) {
        self.break_chain();
        if self.wrap {
            let width = self.wrap_width.max(1);
            let (x, y) = self.cursor;
            let row = visual_row_of(&self.lines[y], x, width);
            let (start, _) = visual_chunk(&self.lines[y], row, width);
            self.cursor.0 = start;
            return;
        }
        self.cursor.0 = 0;
    }

    pub fn end(&mut self) {
        self.break_chain();
        if self.wrap {
            let width = self.wrap_width.max(1);
            let (x, y) = self.cursor;
            let row = visual_row_of(&self.lines[y], x, width);
            let (_, end) = visual_chunk(&self.lines[y], row, width);
            // never rest on whitespace elided at the wrap point
            self.cursor.0 = snap_visible_back(&self.lines[y], end, width);
            return;
        }
        self.cursor.0 = self.line_len(self.cursor.1);
    }

    pub fn page_up(&mut self, rows: usize) {
        self.break_chain();
        if rows == 0 {
            return;
        }
        if self.wrap {
            let width = self.wrap_width.max(1);
            let vrow = self.cursor_vrow().saturating_sub(rows);
            if let Some((y, k)) = self.vrow_position(vrow) {
                let (start, end) = visual_chunk(&self.lines[y], k, width);
                let nx = snap_visible_back(&self.lines[y], self.cursor.0.clamp(start, end), width);
                self.cursor = (nx, y);
            }
            return;
        }
        self.cursor.1 = self.cursor.1.saturating_sub(rows);
        self.clamp_x();
    }

    pub fn page_down(&mut self, rows: usize) {
        self.break_chain();
        if rows == 0 {
            return;
        }
        if self.wrap {
            let width = self.wrap_width.max(1);
            let vrow = (self.cursor_vrow() + rows).min(self.total_visual_rows().saturating_sub(1));
            if let Some((y, k)) = self.vrow_position(vrow) {
                let (start, end) = visual_chunk(&self.lines[y], k, width);
                let nx = snap_visible_back(&self.lines[y], self.cursor.0.clamp(start, end), width);
                self.cursor = (nx, y);
            }
            return;
        }
        let last = self.lines.len() - 1;
        self.cursor.1 = (self.cursor.1 + rows).min(last);
        self.clamp_x();
    }
}
