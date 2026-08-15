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
        }
    }

    /// Load a file into a new buffer. Splitting on `'\n'` and joining with
    /// `'\n'` on save round-trips files byte-for-byte, with or without a
    /// trailing newline (a trailing `'\n'` yields a final empty line).
    pub fn from_path(path: PathBuf) -> io::Result<Self> {
        let content = fs::read_to_string(&path)?;
        let mut buf = Self::empty();
        buf.lines = content.split('\n').map(str::to_string).collect();
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
        self.dirty = false;
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

    // ---- editing -----------------------------------------------------------

    pub fn insert_char(&mut self, c: char) {
        let (x, y) = self.cursor;
        let line = &mut self.lines[y];
        let byte = char_index_to_byte(line, x);
        line.insert(byte, c);
        self.cursor.0 += 1;
        self.dirty = true;
    }

    pub fn insert_text(&mut self, text: &str) {
        for c in text.chars() {
            self.insert_char(c);
        }
    }

    pub fn newline(&mut self) {
        let (x, y) = self.cursor;
        let line = self.lines[y].clone();
        let byte = char_index_to_byte(&line, x);
        let (left, right) = line.split_at(byte);
        self.lines[y] = left.to_string();
        self.lines.insert(y + 1, right.to_string());
        self.cursor = (0, y + 1);
        self.dirty = true;
    }

    pub fn backspace(&mut self) {
        let (x, y) = self.cursor;
        if x > 0 {
            let line = &mut self.lines[y];
            let byte = char_index_to_byte(line, x - 1);
            line.remove(byte);
            self.cursor.0 -= 1;
        } else if y > 0 {
            let prev_len = self.line_len(y - 1);
            let rest = self.lines.remove(y);
            self.lines[y - 1].push_str(&rest);
            self.cursor = (prev_len, y - 1);
        } else {
            return;
        }
        self.dirty = true;
    }

    pub fn delete(&mut self) {
        let (x, y) = self.cursor;
        if x < self.line_len(y) {
            let line = &mut self.lines[y];
            let byte = char_index_to_byte(line, x);
            line.remove(byte);
        } else if y + 1 < self.lines.len() {
            let rest = self.lines.remove(y + 1);
            self.lines[y].push_str(&rest);
        } else {
            return;
        }
        self.dirty = true;
    }

    // ---- cursor movement ---------------------------------------------------

    pub fn move_left(&mut self) {
        if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
        } else if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
            self.cursor.0 = self.line_len(self.cursor.1);
        }
    }

    pub fn move_right(&mut self) {
        let len = self.line_len(self.cursor.1);
        if self.cursor.0 < len {
            self.cursor.0 += 1;
        } else if self.cursor.1 + 1 < self.lines.len() {
            self.cursor.1 += 1;
            self.cursor.0 = 0;
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
            self.clamp_x();
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor.1 + 1 < self.lines.len() {
            self.cursor.1 += 1;
            self.clamp_x();
        }
    }

    pub fn home(&mut self) {
        self.cursor.0 = 0;
    }

    pub fn end(&mut self) {
        self.cursor.0 = self.line_len(self.cursor.1);
    }

    pub fn page_up(&mut self, rows: usize) {
        if rows > 0 {
            self.cursor.1 = self.cursor.1.saturating_sub(rows);
            self.clamp_x();
        }
    }

    pub fn page_down(&mut self, rows: usize) {
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

    /// The text of line `y` restricted to the visible slice, as a `String`.
    pub fn visible_line(&self, y: usize, width: usize) -> String {
        self.lines[y].chars().skip(self.scroll.0).take(width).collect()
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
}
