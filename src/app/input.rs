//! Keyboard routing, shortcut dispatch, clipboard actions, and undo/redo commands.

use super::*;

impl App {
    // ---- key handling ------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) {
        // Global shortcuts work everywhere, even inside the save-as prompt.
        // On macOS, Cmd+key is reported as SUPER on terminals that speak the
        // kitty keyboard protocol; plain Ctrl+key works everywhere.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::SUPER);
        if ctrl {
            // Every Ctrl/Cmd shortcut routes through the same action as
            // its clickable button in the top bar, so the keyboard and the
            // mouse always behave identically.
            let action = match key.code {
                KeyCode::Char('q') => Shortcut::Quit,
                KeyCode::Char('o') => Shortcut::SwitchFocus,
                KeyCode::Char('b') => Shortcut::ToggleSidebar,
                KeyCode::Char('s') => Shortcut::Save,
                KeyCode::Char('r') => Shortcut::Reload,
                KeyCode::Char('c') => Shortcut::Copy,

                KeyCode::Char('x') => Shortcut::Cut,
                KeyCode::Char('v') => Shortcut::Paste,
                KeyCode::Char('a') => Shortcut::SelectAll,
                KeyCode::Char('f') => Shortcut::Find,
                KeyCode::Char('g') => Shortcut::GoToLine,
                KeyCode::Char('w') => Shortcut::ToggleWrap,
                // Ctrl+Shift+H is find-and-replace; unshifted Ctrl+H stays
                // hide-dotfiles. Ghostty (kitty protocol + REPORT_ALTERNATE_KEYS)
                // folds Shift into the character and drops the SHIFT modifier,
                // so the event is 'H'+CONTROL — the same shape as Ctrl+Shift+Z.
                // Accept 'H', or 'h'+Shift, as replace. Bare 'h' is hidden.
                KeyCode::Char('h') | KeyCode::Char('H')
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.code == KeyCode::Char('H') =>
                {
                    Shortcut::Replace
                }
                KeyCode::Char('h') => Shortcut::ToggleHidden,
                KeyCode::Char('n') => Shortcut::NewFile,
                // Ctrl+Z undoes, Ctrl+Shift+Z redoes (CapsLock typos land
                // on redo, a harmless no-op without history). The shifted
                // letter may arrive as 'Z' or as 'z'+Shift depending on
                // the terminal, so accept both.
                KeyCode::Char('z') | KeyCode::Char('Z')
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.code == KeyCode::Char('Z') =>
                {
                    Shortcut::Redo
                }
                KeyCode::Char('z') => Shortcut::Undo,
                _ => return,
            };
            self.invoke_shortcut(action);
            return;
        }

        // An image preview keeps the editor read-only, but it must not make
        // the sidebar modal: Ctrl+O can move focus here and ordinary sidebar
        // navigation (including opening another entry) must continue to work.
        if self.image.is_some() {
            if key.code == KeyCode::Esc {
                self.invoke_shortcut(Shortcut::ClosePreview);
            } else if self.focus == Focus::Sidebar {
                self.handle_sidebar_key(key);
            }
            return;
        }

        if let Some(input) = self.save_as_input.as_mut() {
            if key.code != KeyCode::Enter {
                self.save_as_overwrite = None;
                self.save_as_error = None;
            }
            match key.code {
                KeyCode::Esc => self.save_as_input = None,
                KeyCode::Enter => self.confirm_save_as(),
                KeyCode::Backspace => {
                    input.pop();
                }
                // Ctrl/Super combinations already returned above, so any
                // remaining Char is printable input — including Shift- and
                // Alt/Option-modified characters, which international layouts
                // need (e.g. `[` is Option+5 on a German macOS keyboard).
                KeyCode::Char(c) => input.push(printable_char(c, key.modifiers)),
                _ => {}
            }
            return;
        }

        if let Some(input) = self.goto_line_input.as_mut() {
            match key.code {
                KeyCode::Esc => self.goto_line_input = None,
                KeyCode::Enter => self.confirm_goto_line(),
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) => input.push(printable_char(c, key.modifiers)),
                _ => {}
            }
            return;
        }

        // The search bar is modal like the save-as prompt: Esc closes it,
        // Enter/Shift+Enter step through the matches, and printable input
        // edits the query. Navigation keys dismiss the bar and continue to
        // the focused panel, like clicking in the editor/sidebar does. This
        // matters in particular after Enter: search remains open after
        // stepping through a match, so swallowing Up here would make the
        // editor appear to stop moving vertically until Esc was pressed.
        if self.search.is_some() && key.code == KeyCode::Esc {
            self.search = None;
            return;
        }
        if self.search.is_some()
            && matches!(
                key.code,
                KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::PageUp
                    | KeyCode::PageDown
            )
        {
            self.search = None;
        }
        // Find-and-replace: Tab switches fields; Enter on the find field
        // moves to the replacement; Enter on the replacement replaces the
        // current match (Shift+Enter replaces all). These arms call back
        // into App, so they must not hold a borrow of `search`.
        if self.search.as_ref().is_some_and(|s| s.is_replace()) {
            let field = self.search.as_ref().unwrap().field;
            match key.code {
                KeyCode::Tab => {
                    if let Some(search) = self.search.as_mut() {
                        search.field = match search.field {
                            SearchField::Query => SearchField::Replacement,
                            SearchField::Replacement => SearchField::Query,
                        };
                    }
                    return;
                }
                KeyCode::Enter
                    if field == SearchField::Query
                        && !key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    if let Some(search) = self.search.as_mut() {
                        search.field = SearchField::Replacement;
                    }
                    return;
                }
                KeyCode::Enter if field == SearchField::Replacement => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        self.replace_all_matches();
                    } else {
                        self.replace_current_match();
                    }
                    return;
                }
                _ => {}
            }
        }
        if let Some(search) = self.search.as_mut() {
            let replacing = search.is_replace();
            let field = search.field;
            let mut query_changed = false;
            match key.code {
                KeyCode::Enter => {
                    let dir = if key.modifiers.contains(KeyModifiers::SHIFT) {
                        -1
                    } else {
                        1
                    };
                    search.step(dir);
                }
                KeyCode::Backspace if replacing && field == SearchField::Replacement => {
                    if let Some(replacement) = search.replacement.as_mut() {
                        replacement.pop();
                    }
                    return;
                }
                KeyCode::Backspace => {
                    search.query.pop();
                    query_changed = true;
                }
                KeyCode::Char(c)
                    if replacing && field == SearchField::Replacement && !c.is_control() =>
                {
                    if let Some(replacement) = search.replacement.as_mut() {
                        replacement.push(printable_char(c, key.modifiers));
                    }
                    return;
                }
                KeyCode::Char(c) if !c.is_control() => {
                    search.query.push(printable_char(c, key.modifiers));
                    query_changed = true;
                }
                _ => return,
            }
            if query_changed {
                self.recompute_search();
            } else {
                self.jump_to_current_match();
            }
            return;
        }

        match self.focus {
            Focus::Sidebar => self.handle_sidebar_key(key),
            Focus::Editor => self.handle_editor_key(key),
        }
    }

    /// Run a shortcut action, whether it came from a key or from a click
    /// on its top-bar button. The guards mirror the original key handling:
    /// modal prompts (save-as, search) and image previews keep the
    /// irrelevant actions inert.
    pub(super) fn invoke_shortcut(&mut self, action: Shortcut) {
        match action {
            Shortcut::Quit => {
                if self.buffer.dirty && !self.quit_armed {
                    self.quit_armed = true;
                    self.set_message("unsaved changes — press Ctrl+Q again to quit anyway");
                } else {
                    self.should_quit = true;
                }
            }
            Shortcut::SwitchFocus => {
                // not while a text prompt is modal
                if !self.text_prompt_active() {
                    if !self.sidebar_visible {
                        // Hidden sidebar: show it and focus it so Ctrl+O
                        // never traps the user on a panel they cannot see.
                        self.sidebar_visible = true;
                        self.focus = Focus::Sidebar;
                    } else {
                        self.focus = match self.focus {
                            Focus::Sidebar => Focus::Editor,
                            Focus::Editor => Focus::Sidebar,
                        };
                    }
                }
            }
            Shortcut::ToggleSidebar => self.toggle_sidebar(),
            Shortcut::ClosePreview => {
                if self.image.is_some() {
                    self.close_image_preview();
                }
            }
            Shortcut::NewFile => {
                // not while the save-as prompt or the search bar is modal
                if !self.text_prompt_active() && self.search.is_none() {
                    self.new_file();
                }
            }
            // Reload works while an image preview is open too: it re-reads
            // the image file from disk.
            Shortcut::Reload => self.reload_from_disk(),
            Shortcut::ToggleHidden => self.toggle_hidden_files(),
            // While an image preview is open the remaining shortcuts do
            // nothing: there is no text to edit, save or search. This arm
            // comes after Quit/SwitchFocus/ClosePreview/NewFile/ToggleSidebar
            // (which still work) and before the rest.
            _ if self.image.is_some() => {}
            Shortcut::Save => self.save(),
            Shortcut::Copy => {
                if !self.text_prompt_active() && self.search.is_none() {
                    self.copy_selection();
                }
            }
            Shortcut::Cut => {
                if !self.text_prompt_active() && self.search.is_none() {
                    self.cut_selection();
                }
            }
            Shortcut::Paste => self.paste_clipboard(),
            Shortcut::SelectAll => {
                if !self.text_prompt_active() && self.search.is_none() {
                    self.buffer.select_all();
                }
            }
            Shortcut::Find => self.open_search(),
            Shortcut::Replace => self.open_replace(),
            Shortcut::GoToLine => self.open_goto_line(),
            Shortcut::ToggleWrap => {
                let (w, _) = self.editor_text;
                let wrap = self.buffer.toggle_wrap(w as usize);
                self.set_message(if wrap {
                    "word wrap on"
                } else {
                    "word wrap off"
                });
                self.ensure_cursor_visible();
            }
            Shortcut::Undo => {
                if !self.text_prompt_active() && self.search.is_none() {
                    self.undo();
                }
            }
            Shortcut::Redo => {
                if !self.text_prompt_active() && self.search.is_none() {
                    self.redo();
                }
            }
        }
    }

    pub(super) fn handle_sidebar_key(&mut self, key: KeyEvent) {
        let page = self.sidebar_height.saturating_sub(2) as i64;
        match key.code {
            KeyCode::Up => self.sidebar.move_selection(-1),
            KeyCode::Down => self.sidebar.move_selection(1),
            KeyCode::Home => self.sidebar.selected = 0,
            KeyCode::End => {
                self.sidebar.selected = self.sidebar.entries.len().saturating_sub(1);
            }
            KeyCode::PageUp => self.sidebar.move_selection(-page.max(1)),
            KeyCode::PageDown => self.sidebar.move_selection(page.max(1)),
            KeyCode::Enter | KeyCode::Right => self.open_selected(),
            KeyCode::Backspace | KeyCode::Left => match self.sidebar.ascend() {
                Ok(()) => {}
                Err(e) => self.set_message(format!("cannot go up: {e}")),
            },
            _ => {}
        }
    }

    pub(super) fn handle_editor_key(&mut self, key: KeyEvent) {
        let (w, h) = self.editor_text;
        let page = h.max(1) as usize;
        // shift+arrows extend the selection, plain arrows drop it
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let is_move = matches!(
            key.code,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
        );
        if is_move {
            if shift {
                if !self.buffer.selecting {
                    self.buffer.begin_selection();
                }
            } else {
                self.buffer.clear_selection();
            }
        }
        match key.code {
            // Printable characters insert even when Shift or Alt/Option are
            // held (e.g. `[` and `]` are Option+5 / Option+6 on a German
            // macOS keyboard). Some terminals report Shift+letter as the base
            // key plus a SHIFT modifier instead of the shifted character, so
            // fold ASCII letters to uppercase. Ctrl/Super combinations are
            // reserved for shortcuts and already returned in handle_key
            // before reaching this point.
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
            {
                if !c.is_control() {
                    self.buffer.insert_char(printable_char(c, key.modifiers));
                }
            }
            KeyCode::Enter => self.buffer.newline(),
            KeyCode::Tab if shift => self.buffer.dedent(),
            KeyCode::Tab => self.buffer.indent(),
            KeyCode::BackTab => self.buffer.dedent(),
            KeyCode::Backspace => self.buffer.backspace(),
            KeyCode::Delete => self.buffer.delete(),
            KeyCode::Left => self.buffer.move_left(),
            KeyCode::Right => self.buffer.move_right(),
            KeyCode::Up => self.buffer.move_up(),
            KeyCode::Down => self.buffer.move_down(),
            KeyCode::Home => self.buffer.home(),
            KeyCode::End => self.buffer.end(),
            KeyCode::PageUp => self.buffer.page_up(page),
            KeyCode::PageDown => self.buffer.page_down(page),
            _ => {}
        }
        self.quit_armed = false;
        if is_move {
            self.buffer.sync_parinfer_cursor();
        } else if matches!(
            key.code,
            KeyCode::Char(_)
                | KeyCode::Enter
                | KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::Backspace
                | KeyCode::Delete
        ) {
            self.buffer.apply_parinfer();
        }
        // invalidate the highlight cache at the first changed line
        if let Some(line) = self.buffer.last_edit_line.take() {
            self.highlighter.invalidate_from(line);
        }
        self.buffer.ensure_visible(h as usize, w as usize);
    }

    // ---- undo / redo ------------------------------------------------------

    pub(super) fn undo(&mut self) {
        if self.buffer.undo() {
            self.after_undo_redo();
        }
    }

    pub(super) fn redo(&mut self) {
        if self.buffer.redo() {
            self.after_undo_redo();
        }
    }

    /// Re-highlight the changed lines, keep the restored cursor visible,
    /// and re-arm the quit guard (the buffer changed again).
    pub(super) fn after_undo_redo(&mut self) {
        if let Some(line) = self.buffer.last_edit_line.take() {
            self.highlighter.invalidate_from(line);
        }
        let (w, h) = self.editor_text;
        self.buffer.ensure_visible(h as usize, w as usize);
        self.quit_armed = false;
    }

    // ---- clipboard ---------------------------------------------------------

    pub(super) fn copy_selection(&mut self) {
        let Some(text) = self.buffer.selected_text() else {
            return;
        };
        if let Err(error) = self.clipboard.set_text(&text) {
            self.set_message(format!("clipboard failed: {error}"));
            return;
        }
        self.set_message("copied");
    }

    pub(super) fn cut_selection(&mut self) {
        if !self.buffer.has_selection() {
            return;
        }
        let text = self.buffer.selected_text().unwrap_or_default();
        if let Err(error) = self.clipboard.set_text(&text) {
            self.set_message(format!("clipboard failed: {error}"));
            return;
        }
        self.buffer.delete_selection();
        self.buffer.apply_parinfer();
        if let Some(line) = self.buffer.last_edit_line.take() {
            self.highlighter.invalidate_from(line);
        }
        self.set_message("cut");
    }

    pub(super) fn paste_clipboard(&mut self) {
        let Some(text) = self.clipboard.get_text() else {
            self.set_message("clipboard unavailable");
            return;
        };
        if text.is_empty() {
            return;
        }
        if let Some(input) = self.save_as_input.as_mut() {
            self.save_as_overwrite = None;
            self.save_as_error = None;
            input.push_str(&text);
            return;
        }
        if let Some(input) = self.goto_line_input.as_mut() {
            input.push_str(&text);
            return;
        }
        if self.search.is_some() {
            self.append_to_search(&text);
            return;
        }
        self.paste_text(text);
    }

    /// Insert pasted text (from Ctrl+V or bracketed paste) into the buffer.
    pub fn paste_text(&mut self, text: String) {
        // pasting while an image preview is open would edit an invisible
        // buffer, so ignore it
        if self.image.is_some() {
            return;
        }
        // bracketed paste while a text prompt is open fills the prompt
        if let Some(input) = self.save_as_input.as_mut() {
            self.save_as_overwrite = None;
            self.save_as_error = None;
            input.extend(text.chars().filter(|c| !c.is_control()));
            return;
        }
        if let Some(input) = self.goto_line_input.as_mut() {
            input.extend(text.chars().filter(|c| !c.is_control()));
            return;
        }
        // bracketed paste while searching fills in the active field
        if self.search.is_some() {
            self.append_to_search(&text);
            return;
        }
        self.focus = Focus::Editor;
        self.buffer.insert_multiline(&text);
        self.buffer.apply_parinfer();
        if let Some(line) = self.buffer.last_edit_line.take() {
            self.highlighter.invalidate_from(line);
        }
        self.quit_armed = false;
    }
}
