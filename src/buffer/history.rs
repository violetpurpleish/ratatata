//! Undo snapshots, edit coalescing, and clean-state restoration.

use super::*;

impl Buffer {
    // ---- undo / redo ------------------------------------------------------

    /// Save the current state as an undo step (dropping the oldest when
    /// the history is full) and discard any redo history.
    pub(super) fn push_undo(&mut self, kind: EditKind) {
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
    pub(super) fn mergeable(&self, kind: EditKind) -> bool {
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
            | (EditKind::Newline, EditKind::Newline)
            | (EditKind::Indent, EditKind::Indent)
            | (EditKind::Dedent, EditKind::Dedent) => self.cursor == last_pos,
            _ => false,
        }
    }

    /// Break the merge chain: the next edit starts a fresh undo step.
    /// Called after cursor movement, selection changes and saves.
    pub(super) fn break_chain(&mut self) {
        self.last_edit = None;
    }

    pub(super) fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            cursor: self.cursor,
            selection_anchor: self.selection_anchor,
            selecting: self.selecting,
            dirty: self.dirty,
        }
    }

    pub(super) fn restore(&mut self, snap: Snapshot) {
        self.lines = snap.lines;
        self.cursor = snap.cursor;
        self.selection_anchor = snap.selection_anchor;
        self.selecting = snap.selecting;
        self.dirty = snap.dirty;
        self.sync_parinfer_prev();
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
}
