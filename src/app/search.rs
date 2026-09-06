//! Search, replacement, and go-to-line prompt behavior.

use super::*;

impl App {
    // ---- search ------------------------------------------------------------

    /// Open the search bar. Ctrl+F while it is already open jumps to the
    /// next match (like most editors).
    pub(super) fn open_search(&mut self) {
        if self.save_as_input.is_some() {
            return;
        }
        self.goto_line_input = None;
        if self.search.is_some() {
            self.search_step(1);
        } else {
            self.search = Some(Search::new());
        }
    }

    /// Open find-and-replace. Reuses an existing Ctrl+F search when one is
    /// already open (keeping the query) so a newcomer can type find, then
    /// type the replacement.
    pub(super) fn open_replace(&mut self) {
        if self.save_as_input.is_some() {
            return;
        }
        self.goto_line_input = None;
        match &mut self.search {
            Some(search) => search.enable_replace(),
            None => self.search = Some(Search::new_replace()),
        }
    }

    /// Append pasted text to whichever search field is being edited.
    pub(super) fn append_to_search(&mut self, text: &str) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let filtered = text.chars().filter(|c| !c.is_control());
        if search.is_replace() && search.field == SearchField::Replacement {
            if let Some(replacement) = search.replacement.as_mut() {
                replacement.extend(filtered);
            }
            return;
        }
        search.query.extend(filtered);
        self.recompute_search();
    }

    /// Replace the current match with the replacement string and jump to
    /// the next remaining match. No-ops with a status message when there
    /// is nothing to replace.
    pub(super) fn replace_current_match(&mut self) {
        let Some(search) = self.search.as_ref() else {
            return;
        };
        let Some(replacement) = search.replacement.clone() else {
            return;
        };
        let Some(m) = search.current_match() else {
            self.set_message("no matches");
            return;
        };
        self.buffer
            .replace_line_range(m.line, m.start, m.end, &replacement);
        self.buffer.apply_parinfer();
        if let Some(line) = self.buffer.last_edit_line.take() {
            self.highlighter.invalidate_from(line);
        }
        self.quit_armed = false;
        self.recompute_search();
    }

    /// Replace every current match. One undo step. If the replacement
    /// contains the query, only the original matches are replaced (last
    /// to first) so this cannot loop.
    pub(super) fn replace_all_matches(&mut self) {
        let Some(search) = self.search.as_ref() else {
            return;
        };
        let Some(replacement) = search.replacement.clone() else {
            return;
        };
        let ranges: Vec<(usize, usize, usize)> = search
            .matches()
            .iter()
            .map(|m| (m.line, m.start, m.end))
            .collect();
        if ranges.is_empty() {
            self.set_message("no matches");
            return;
        }
        let n = ranges.len();
        self.buffer.replace_line_ranges(&ranges, &replacement);
        self.buffer.apply_parinfer();
        if let Some(line) = self.buffer.last_edit_line.take() {
            self.highlighter.invalidate_from(line);
        }
        self.quit_armed = false;
        self.recompute_search();
        self.set_message(format!(
            "replaced {n} {}",
            if n == 1 { "match" } else { "matches" }
        ));
    }

    /// Open the go-to-line prompt. Invalid numbers are reported in the
    /// status bar rather than panicking.
    pub(super) fn open_goto_line(&mut self) {
        if self.save_as_input.is_some() {
            return;
        }
        self.search = None;
        if self.goto_line_input.is_none() {
            self.goto_line_input = Some(String::new());
        }
    }

    pub(super) fn confirm_goto_line(&mut self) {
        let Some(input) = self.goto_line_input.take() else {
            return;
        };
        let trimmed = input.trim();
        if trimmed.is_empty() {
            self.set_message("invalid line number");
            return;
        }
        let last = self.buffer.lines.len();
        match trimmed.parse::<usize>() {
            Ok(n) if n >= 1 && n <= last => {
                let y = n - 1;
                self.buffer.clear_selection();
                self.buffer.cursor = (0, y);
                self.buffer.sync_parinfer_cursor();
                self.focus = Focus::Editor;
                self.ensure_cursor_visible();
            }
            Ok(_) => self.set_message(format!("line {trimmed} is out of range (1–{last})")),
            Err(_) => self.set_message("invalid line number"),
        }
    }

    /// Move the current match by `dir` (+1 next, −1 previous) and jump the
    /// cursor to it.
    pub(super) fn search_step(&mut self, dir: isize) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        search.step(dir);
        self.jump_to_current_match();
    }

    /// Recompute the matches after the query changed and jump the cursor to
    /// the match at or after its current position.
    pub(super) fn recompute_search(&mut self) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let cursor = self.buffer.cursor;
        search.refresh(&self.buffer.lines, cursor);
        self.jump_to_current_match();
    }

    /// Move the cursor onto the current search match and clear any
    /// selection so the highlight reads cleanly.
    pub(super) fn jump_to_current_match(&mut self) {
        let Some(search) = self.search.as_ref() else {
            return;
        };
        let Some(m) = search.current_match() else {
            return;
        };
        self.buffer.cursor = (m.start, m.line);
        self.buffer.clear_selection();
        self.buffer.sync_parinfer_cursor();
        self.ensure_cursor_visible();
    }
}
