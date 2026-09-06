//! File lifecycle, save-as prompts, reloads, and image-preview restoration.

use super::*;

impl App {
    // ---- file operations ---------------------------------------------------

    /// Drop the current buffer (and any image preview) and start a fresh
    /// untitled one — the same state as launching the app from a directory
    /// without a file path, except that focus stays on the editor so the
    /// new file can be typed into immediately. Refuses while the buffer
    /// has unsaved changes.
    pub(super) fn new_file(&mut self) {
        if self.buffer.dirty {
            self.set_message("unsaved changes — press Ctrl+S to save first");
            return;
        }
        self.buffer = Buffer::empty();
        self.previous_buffer = None;
        self.image = None;
        self.highlighter.set_path(None);
        self.quit_armed = false;
        self.focus = Focus::Editor;
        self.set_message("new file — press Ctrl+S to save");
    }

    pub(super) fn save(&mut self) {
        if self.save_as_input.is_some() {
            return;
        }
        if self.buffer.path.is_some() {
            match self.buffer.save() {
                Ok(()) => {
                    self.quit_armed = false;
                    let path = self.buffer.path.as_ref().unwrap().display();
                    self.set_message(format!("saved {path}"));
                }
                Err(e) => self.set_message(format!("save failed: {e}")),
            }
        } else {
            // no file name yet: ask for one
            self.save_as_input = Some(String::new());
            self.save_as_overwrite = None;
            self.save_as_error = None;
        }
    }

    pub(super) fn confirm_save_as(&mut self) {
        let Some(input) = self.save_as_input.as_ref() else {
            return;
        };
        let input = input.trim().to_string();
        if input.is_empty() {
            self.save_as_input = None;
            return;
        }
        let path = PathBuf::from(&input);
        let overwrite = self.save_as_overwrite.as_ref() == Some(&path);
        match self.buffer.save_to(path.clone(), overwrite) {
            Ok(()) => {
                self.save_as_input = None;
                self.save_as_overwrite = None;
                self.save_as_error = None;
                self.buffer.sync_parinfer_prev();
                self.highlighter.set_path(Some(&path));
                self.quit_armed = false;
                if let Err(e) = self.sidebar.reload() {
                    self.set_message(format!(
                        "saved {}, but sidebar refresh failed: {e}",
                        path.display()
                    ));
                    return;
                }
                self.set_message(format!("saved {}", path.display()));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && !path.is_dir() => {
                self.save_as_overwrite = Some(path);
                self.save_as_error = None;
            }
            Err(e) => {
                self.save_as_overwrite = None;
                self.save_as_error = Some(format!("save failed: {e}; edit path: "));
            }
        }
    }

    /// Re-read the currently open file — the text buffer or the image
    /// preview — from disk, and refresh the sidebar listing so files that
    /// were added, removed or renamed show up. The reload is refused while
    /// the buffer has unsaved changes (they would be lost), but the sidebar
    /// still refreshes in every case.
    pub(super) fn reload_from_disk(&mut self) {
        let message: Option<String>;

        // an open image preview re-decodes its file
        let image_path = self.image.as_ref().map(|preview| preview.path.clone());
        if let Some(path) = image_path {
            match ImagePreview::open_with_cell_size(
                path.clone(),
                &self.picker,
                self.logical_cell_size,
            ) {
                Ok(updated) => {
                    self.image = Some(updated);
                    message = Some(format!("reloaded {}", path.display()));
                }
                Err(e) => message = Some(format!("cannot reload {}: {e}", path.display())),
            }
        } else if self.buffer.dirty {
            message = Some("unsaved changes — press Ctrl+S to save first".to_string());
        } else if let Some(path) = self.buffer.path.clone() {
            // Preserve the viewport, cursor and wrap preference, so the
            // reload shows the new content where the old one was.
            let cursor = self.buffer.cursor;
            let scroll = self.buffer.scroll;
            let wrap = self.buffer.wrap;
            let wrap_width = self.buffer.wrap_width;
            match Buffer::from_path(path.clone()) {
                Ok(mut buffer) => {
                    buffer.cursor = cursor;
                    buffer.scroll = scroll;
                    if wrap {
                        buffer.wrap = true;
                        buffer.wrap_width = wrap_width;
                    }
                    self.buffer = buffer;
                    self.highlighter.set_path(Some(&path));
                    self.quit_armed = false;
                    // search matches were computed against the old text
                    if self.search.is_some() {
                        self.recompute_search();
                    }
                    let (w, h) = self.editor_text;
                    self.buffer.ensure_visible(h as usize, w as usize);
                    // Cursor may have been out of range if the file shrank.
                    // Remember Parinfer state only after the final clamp.
                    self.buffer.sync_parinfer_prev();
                    message = Some(format!("reloaded {}", path.display()));
                }
                Err(e) => message = Some(format!("cannot reload {}: {e}", path.display())),
            }
        } else {
            message = Some("no file to reload".to_string());
        }

        // The sidebar refresh happens even when the file reload was
        // refused or failed, so external changes always show up in the
        // listing.
        if let Err(e) = self.sidebar.reload() {
            self.set_message(format!("sidebar refresh failed: {e}"));
            return;
        }
        if let Some(msg) = message {
            self.set_message(msg);
        }
    }

    pub(super) fn open_selected(&mut self) {
        let Some(path) = self.sidebar.selected_path() else {
            return;
        };
        if self.sidebar.selected_is_dir() {
            match self.sidebar.descend() {
                Ok(()) => {}
                Err(e) => self.set_message(format!("cannot open {}: {e}", path.display())),
            }
            return;
        }
        if self.buffer.dirty {
            self.set_message("unsaved changes — press Ctrl+S to save first");
            return;
        }
        if image_view::is_image_path(&path) {
            match ImagePreview::open_with_cell_size(
                path.clone(),
                &self.picker,
                self.logical_cell_size,
            ) {
                Ok(preview) => {
                    self.stash_buffer_for_preview();
                    self.image = Some(preview);
                    self.quit_armed = false;
                    self.focus = Focus::Editor;
                    self.set_message(format!("previewing {}", path.display()));
                }
                Err(e) => self.set_message(format!("cannot open {}: {e}", path.display())),
            }
            return;
        }
        match Buffer::from_path(path.clone()) {
            Ok(buffer) => {
                self.previous_buffer = None;
                self.image = None;
                self.buffer = buffer;
                self.highlighter.set_path(Some(&path));
                self.quit_armed = false;
                self.focus = Focus::Editor;
                self.set_message(format!("opened {}", path.display()));
            }
            Err(e) => self.set_message(format!("cannot open {}: {e}", path.display())),
        }
    }

    /// Put the current text buffer aside for the duration of an image
    /// preview. Opening another image while already previewing keeps the
    /// original text buffer so Esc still restores it.
    pub(super) fn stash_buffer_for_preview(&mut self) {
        if self.image.is_none() {
            self.previous_buffer = Some(std::mem::replace(&mut self.buffer, Buffer::empty()));
        } else {
            self.buffer = Buffer::empty();
        }
    }

    /// Close the image preview and restore the previously open buffer
    /// (including wrap). With no stashed buffer this lands on empty untitled,
    /// which is the startup-preview case.
    pub(super) fn close_image_preview(&mut self) {
        self.image = None;
        if let Some(buffer) = self.previous_buffer.take() {
            self.highlighter.set_path(buffer.path.as_deref());
            self.buffer = buffer;
            self.last_drawn_cursor = None;
        } else {
            self.buffer = Buffer::empty();
            self.highlighter.set_path(None);
        }
        self.focus = Focus::Editor;
    }

    /// Toggle whether the sidebar lists dotfiles. `..` is never hidden.
    pub(super) fn toggle_hidden_files(&mut self) {
        self.sidebar.hide_dotfiles = !self.sidebar.hide_dotfiles;
        let selected = self
            .sidebar
            .entries
            .get(self.sidebar.selected)
            .map(|e| e.name.clone());
        if let Err(e) = self.sidebar.reload() {
            self.set_message(format!("cannot refresh sidebar: {e}"));
            return;
        }
        if let Some(name) = selected {
            self.sidebar.select_name(&name);
        }
        self.set_message(if self.sidebar.hide_dotfiles {
            "dotfiles hidden"
        } else {
            "dotfiles shown"
        });
    }

    /// Hide or show the file tree. Directory and selection survive so the
    /// editor can take the width and toggling back restores the tree.
    pub(super) fn toggle_sidebar(&mut self) {
        self.sidebar_visible = !self.sidebar_visible;
        if !self.sidebar_visible && self.focus == Focus::Sidebar {
            self.focus = Focus::Editor;
        }
        self.set_message(if self.sidebar_visible {
            "sidebar shown"
        } else {
            "sidebar hidden"
        });
    }
}
