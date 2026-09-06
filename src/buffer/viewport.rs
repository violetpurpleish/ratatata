//! Soft-wrap coordinates, scrolling, and cursor visibility.

use super::*;

impl Buffer {
    // ---- soft wrapping -----------------------------------------------------

    /// Toggle soft wrapping of long lines and return the new state.
    /// `width` is the terminal column width to wrap at (the app refreshes
    /// it on every draw, so the value only matters until then). When
    /// leaving wrap mode the viewport is mapped back from visual rows to
    /// logical lines so it does not jump past the end of the buffer.
    pub fn toggle_wrap(&mut self, width: usize) -> bool {
        if self.wrap {
            let last = self.lines.len().saturating_sub(1);
            let line = self
                .vrow_position(self.scroll.1)
                .map(|(y, _)| y)
                .unwrap_or(last);
            self.scroll.1 = line.min(last);
            self.wrap = false;
        } else {
            self.wrap_width = width.max(1);
            // horizontal scrolling is meaningless while wrapping: the
            // caret column math assumes scroll.x == 0
            self.scroll.0 = 0;
            self.wrap = true;
        }
        self.wrap
    }

    /// Visual row (0-based) of the cursor: with wrapping, its row within
    /// its line plus the rows of all preceding lines; without wrapping,
    /// simply its line number.
    pub fn cursor_vrow(&self) -> usize {
        if !self.wrap {
            return self.cursor.1;
        }
        let width = self.wrap_width.max(1);
        let mut vrow = 0;
        for (y, line) in self.lines.iter().enumerate() {
            if y == self.cursor.1 {
                return vrow + visual_row_of(line, self.cursor.0, width);
            }
            vrow += visual_len(line, width);
        }
        vrow
    }

    /// `(logical line, chunk index)` of the visual row `vrow`, or `None`
    /// past the end of the buffer. Without wrapping the chunk index is
    /// always 0.
    pub fn vrow_position(&self, vrow: usize) -> Option<(usize, usize)> {
        let width = self.wrap_width.max(1);
        let mut remaining = vrow;
        for (y, line) in self.lines.iter().enumerate() {
            let n = if self.wrap {
                visual_len(line, width)
            } else {
                1
            };
            if remaining < n {
                return Some((y, if self.wrap { remaining } else { 0 }));
            }
            remaining -= n;
        }
        None
    }

    /// How many visual rows the whole buffer takes at the current wrap
    /// width (the line count when wrapping is off).
    pub fn total_visual_rows(&self) -> usize {
        if !self.wrap {
            return self.lines.len();
        }
        let width = self.wrap_width.max(1);
        self.lines.iter().map(|l| visual_len(l, width)).sum()
    }

    // ---- scrolling / rendering --------------------------------------------

    /// Clamp the vertical viewport to the content currently in the buffer.
    /// Unlike [`ensure_visible`], this never moves the cursor or otherwise
    /// reanchors the viewport to it; it is used by independent scrolling
    /// (for example, the mouse wheel).
    pub fn clamp_scroll(&mut self, view_h: usize) {
        let view_h = view_h.max(1);
        let max_scroll = self.total_visual_rows().saturating_sub(view_h);
        self.scroll.1 = self.scroll.1.min(max_scroll);
        if self.wrap {
            // horizontal scrolling is meaningless while wrapping
            self.scroll.0 = 0;
        }
    }

    /// Move the vertical viewport without moving the cursor.
    ///
    /// With wrapping, `delta` is measured in visual rows; otherwise it is
    /// measured in logical lines. The offset is clamped so the viewport
    /// never scrolls past the end of the buffer.
    pub fn scroll_vertical(&mut self, delta: isize, view_h: usize) {
        self.clamp_scroll(view_h);
        let view_h = view_h.max(1);
        let max_scroll = self.total_visual_rows().saturating_sub(view_h);
        self.scroll.1 = if delta < 0 {
            self.scroll.1.saturating_sub(delta.unsigned_abs())
        } else {
            self.scroll.1.saturating_add(delta as usize)
        }
        .min(max_scroll);
    }

    /// Scroll so the cursor is inside the visible viewport
    /// (`view_w` x `view_h` chars). With wrapping `scroll.y` is a visual
    /// row and `view_h` counts rows; without wrapping it is a logical
    /// line and `view_w` is the horizontal window.
    pub fn ensure_visible(&mut self, view_h: usize, view_w: usize) {
        let view_h = view_h.max(1);
        let view_w = view_w.max(1);
        self.clamp_scroll(view_h);
        // Guard against an out-of-range cursor (reload of a shorter file,
        // not normal movement). Clamp the line first so `clamp_x` uses the
        // final line's length.
        if self.cursor.1 >= self.lines.len() {
            self.cursor.1 = self.lines.len() - 1;
        }
        self.clamp_x();
        if self.wrap {
            let cv = self.cursor_vrow();
            if cv < self.scroll.1 {
                self.scroll.1 = cv;
            }
            if cv >= self.scroll.1 + view_h {
                self.scroll.1 = cv.saturating_add(1).saturating_sub(view_h);
            }
            // horizontal scrolling is meaningless while wrapping; keep
            // scroll.x at 0 so the caret column math is not offset
            self.scroll.0 = 0;
            return;
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
    /// accounting for wide (CJK etc.) characters. With wrapping this is
    /// the column within the cursor's visual row.
    pub fn cursor_col(&self) -> usize {
        let line = &self.lines[self.cursor.1];
        if self.wrap {
            let width = self.wrap_width.max(1);
            let row = visual_row_of(line, self.cursor.0, width);
            let (start, _) = visual_chunk(line, row, width);
            // a cursor on whitespace elided by the wrap renders at the
            // start of the row that follows it
            if self.cursor.0 < start {
                return 0;
            }
            return line
                .chars()
                .skip(start)
                .take(self.cursor.0 - start)
                .map(char_width)
                .sum();
        }
        let to_cursor: usize = line.chars().take(self.cursor.0).map(char_width).sum();
        let to_scroll: usize = line.chars().take(self.scroll.0).map(char_width).sum();
        to_cursor.saturating_sub(to_scroll)
    }
}
