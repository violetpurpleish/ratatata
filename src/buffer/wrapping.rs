//! Word-boundary wrapping and visual-row calculations.

use super::*;

// ---- soft wrapping --------------------------------------------------------
//
// With wrapping enabled every logical line is split into *visual rows* of
// at most `width` terminal columns. Rows break at word boundaries: the
// row ends after the last whitespace that fits, so words stay whole (a
// single word longer than the width is hard-broken, and wide CJK chars
// are never split). Whitespace that does not fit at a wrap point is
// *elided*: it stays in the logical line but is rendered on no row, and
// continuation rows start at the first visible char. A visual row is a
// char range `[start, end)` of its logical line; `scroll.y` counts
// visual rows instead of lines.

/// Char index where the row starting at char `start` of `line` would
/// end by pure column width (exclusive): the largest index whose
/// display width fits in `width` columns. This is the hard-break
/// fallback used when no whitespace boundary is available; a single
/// character wider than `width` is kept whole (it overflows, but is
/// never split).
fn chunk_end(line: &str, start: usize, width: usize) -> usize {
    let width = width.max(1);
    let mut w = 0;
    for (i, c) in line.chars().enumerate().skip(start) {
        let cw = char_width(c);
        if w + cw > width {
            // the next char starts a new row; if even the first char
            // does not fit, keep it on this row anyway
            return if w == 0 { i + 1 } else { i };
        }
        w += cw;
    }
    line.chars().count()
}

/// Whether `c` counts as word-boundary whitespace for wrapping.
fn is_ws(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// Index of the last whitespace char in `line[start..end)`, if any.
fn last_ws(line: &str, start: usize, end: usize) -> Option<usize> {
    line.chars()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
        .fold(None, |last, (i, c)| if is_ws(c) { Some(i) } else { last })
}

/// Iterator over the visual rows of `line` at `width` columns, yielding
/// `(start_char, end_char)` ranges. An empty line is a single empty row;
/// ranges may be non-contiguous where elided whitespace sits between
/// them.
pub(crate) struct VisualChunks<'a> {
    line: &'a str,
    width: usize,
    start: usize,
    done: bool,
}

impl Iterator for VisualChunks<'_> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<(usize, usize)> {
        if self.done {
            return None;
        }
        let len = self.line.chars().count();
        let start = self.start;
        if start >= len {
            // an empty line, or a trailing run of whitespace elided
            // past the last row: one empty row, then nothing
            self.done = true;
            return Some((start, start));
        }
        // continuation rows skip the whitespace the previous row could
        // not fit (it is elided, not shown at the start of the row)
        let start = if start > 0 {
            let mut s = start;
            while s < len && is_ws(self.line.chars().nth(s).expect("s < len")) {
                s += 1;
            }
            if s >= len {
                self.start = len;
                self.done = true;
                return Some((s, s));
            }
            s
        } else {
            start
        };
        let max_end = chunk_end(self.line, start, self.width);
        // word wrap: end the row after the last whitespace that fits, so
        // the next row starts with a whole word; without a boundary the
        // row hard-breaks at the column width
        let end = if max_end < len {
            last_ws(self.line, start, max_end).map_or(max_end, |ws| ws + 1)
        } else {
            max_end
        };
        self.start = end;
        if end >= len {
            self.done = true;
        }
        Some((start, end))
    }
}

/// The visual rows of `line` at `width` columns, as char ranges.
pub(crate) fn visual_chunks(line: &str, width: usize) -> VisualChunks<'_> {
    VisualChunks {
        line,
        width: width.max(1),
        start: 0,
        done: false,
    }
}

/// How many visual rows `line` takes at `width` columns (1 when it
/// fits, including empty lines).
pub(crate) fn visual_len(line: &str, width: usize) -> usize {
    visual_chunks(line, width).count()
}

/// Char range `[start, end)` of the `k`-th visual row (0-based) of
/// `line` at `width` columns. Past the end the last row is repeated.
pub(crate) fn visual_chunk(line: &str, k: usize, width: usize) -> (usize, usize) {
    visual_chunks(line, width)
        .nth(k)
        .unwrap_or((char_count(line), char_count(line)))
}

/// Index of the visual row of `line` that contains char `x` (the last
/// row when `x` is past the end of the line). A position in an elided
/// gap maps to the row that follows it.
pub(crate) fn visual_row_of(line: &str, x: usize, width: usize) -> usize {
    let mut row = 0;
    for (_, end) in visual_chunks(line, width) {
        if x < end {
            return row;
        }
        row += 1;
    }
    row.saturating_sub(1)
}

/// Snap a position inside an elided gap (whitespace hidden by word
/// wrap) to the previous visible char; other positions pass through.
/// Used when moving up/left so the caret never rests on hidden text.
pub(super) fn snap_visible_back(line: &str, x: usize, width: usize) -> usize {
    let row = visual_row_of(line, x, width);
    let (start, _) = visual_chunk(line, row, width);
    if x < start {
        let (_, prev_end) = visual_chunk(line, row.saturating_sub(1), width);
        prev_end.saturating_sub(1)
    } else {
        x
    }
}

/// Snap a position inside an elided gap to the next visible char (the
/// start of the row that follows); other positions pass through. Used
/// when moving right.
pub(super) fn snap_visible_fwd(line: &str, x: usize, width: usize) -> usize {
    let row = visual_row_of(line, x, width);
    let (start, _) = visual_chunk(line, row, width);
    if x < start { start } else { x }
}
