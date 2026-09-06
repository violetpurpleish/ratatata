//! Smart Mode integration and cursor synchronization for Clojure-family files.

use super::*;

impl Buffer {
    /// Remember the current text and cursor as Parinfer's previous state.
    /// Called after load, undo/redo, and path changes so the next Smart
    /// Mode request does not invent a fake change.
    pub(crate) fn sync_parinfer_prev(&mut self) {
        self.parinfer_prev_text = self.content();
        self.parinfer_prev_cursor = self.cursor;
    }

    /// Remember the current cursor for the next Smart Mode request without
    /// running the engine. Used after pure navigation (arrows, Home/End,
    /// PageUp/PageDown, mouse placement, go-to-line, search jumps, and
    /// select-all) so cursor-only movement cannot mutate the buffer.
    /// Matches upstream parinfer-rust's Vim plugin, which only reprocesses
    /// when `changedtick` shows a text change.
    pub(crate) fn sync_parinfer_cursor(&mut self) {
        if !crate::parinfer::applies_to_path(self.path.as_deref()) {
            return;
        }
        self.parinfer_prev_cursor = self.cursor;
    }

    #[cfg(test)]
    pub(crate) fn parinfer_prev_cursor(&self) -> (usize, usize) {
        self.parinfer_prev_cursor
    }

    /// Run Parinfer Smart Mode after a text edit in a Clojure-family file.
    /// No-ops for other paths. If the buffer text has not changed since the
    /// last request, only the remembered cursor is updated — a plain cursor
    /// move must not rewrite the file. On engine failure the user text is
    /// kept. A successful adjustment that follows a user edit is merged
    /// into that edit's undo step.
    ///
    /// Do not call this after cursor-only navigation; use
    /// [`Self::sync_parinfer_cursor`] instead.
    pub(crate) fn apply_parinfer(&mut self) {
        if !crate::parinfer::applies_to_path(self.path.as_deref()) {
            return;
        }
        let text = self.content();
        if text == self.parinfer_prev_text {
            self.parinfer_prev_cursor = self.cursor;
            return;
        }
        let text_edited =
            self.last_edit_line.is_some() || self.last_edit.is_some() || self.force_merge;
        let input = crate::parinfer::Input {
            text: &text,
            cursor: self.cursor,
            prev_text: &self.parinfer_prev_text,
            prev_cursor: self.parinfer_prev_cursor,
            selection_start_line: self.selection_range().map(|((_, ay), _)| ay),
        };
        let Some(output) = crate::parinfer::smart_edit(&input) else {
            self.parinfer_prev_text = text;
            self.parinfer_prev_cursor = self.cursor;
            return;
        };
        self.apply_parinfer_output(output, text_edited);
    }

    pub(super) fn apply_parinfer_output(
        &mut self,
        output: crate::parinfer::Output,
        merge_undo: bool,
    ) {
        let new_lines: Vec<String> = output.text.split('\n').map(str::to_string).collect();
        let text_changed = new_lines != self.lines;
        if !text_changed {
            self.cursor = output.cursor;
            self.clamp_x();
            if self.cursor.1 >= self.lines.len() {
                self.cursor.1 = self.lines.len() - 1;
                self.clamp_x();
            }
            self.parinfer_prev_text = output.text;
            self.parinfer_prev_cursor = self.cursor;
            return;
        }

        let last_kind = self.last_edit.map(|(kind, _)| kind);
        if !merge_undo {
            self.push_undo(EditKind::Parinfer);
        }

        let old_lines = std::mem::replace(&mut self.lines, new_lines);
        if let Some(anchor) = self.selection_anchor {
            self.selection_anchor = Some(map_pos_across_lines(&old_lines, &self.lines, anchor));
        }
        let first_diff = first_diff_line(&old_lines, &self.lines);
        self.cursor = output.cursor;
        if self.cursor.1 >= self.lines.len() {
            self.cursor.1 = self.lines.len() - 1;
        }
        self.clamp_x();
        self.dirty = true;
        if let Some(y) = first_diff {
            self.mark_edited(y);
        }
        if merge_undo {
            if let Some(kind) = last_kind {
                self.last_edit = Some((kind, self.cursor));
            }
        } else {
            self.last_edit = None;
        }
        self.parinfer_prev_text = self.content();
        self.parinfer_prev_cursor = self.cursor;
    }
}
