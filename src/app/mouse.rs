//! Mouse gestures, hit testing, selection, and viewport scrolling.

use super::*;

impl App {
    // ---- mouse -------------------------------------------------------------

    pub fn handle_mouse(&mut self, event: MouseEvent) {
        let pos = (event.column as usize, event.row as usize);
        // The status bar describes whichever shortcut button the mouse is
        // over. Moved events can be sparse (or absent) on some terminals,
        // so recompute the hover on every event rather than only on Moved.
        self.hovered = self.topbar_action_at(pos);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // clicking a shortcut button runs the action directly
                if self.in_topbar(pos) {
                    if let Some(action) = self.topbar_action_at(pos) {
                        self.invoke_shortcut(action);
                    }
                    return;
                }
                self.quit_armed = false;
                // clicking dismisses the search bar (like most editors)
                self.search = None;
                if self.in_sidebar(pos) {
                    self.focus = Focus::Sidebar;
                    if let Some(row) = self.sidebar_row_at(pos) {
                        self.sidebar.selected = self.sidebar.scroll + row;
                    }
                } else if self.in_editor(pos) {
                    self.focus = Focus::Editor;
                    let click = self.editor_cursor_at(pos);
                    // Ctrl/Cmd-click opens an HTTP(S) or `www.` link under
                    // the pointer instead of entering the normal click/
                    // selection sequence. A non-link Ctrl/Cmd-click keeps
                    // the ordinary cursor-placement behavior.
                    if Self::is_ctrl_or_cmd(event.modifiers)
                        && self.in_editor_text(pos)
                        && self.open_link_at(click)
                    {
                        return;
                    }
                    // consecutive clicks on the same position within
                    // EDITOR_CLICK_TTL count up: 2 = double-click (select
                    // word), 3 = triple-click (select line); a different
                    // position or a pause resets, and a 4th rapid click
                    // starts a fresh sequence (so extra clicks never get
                    // stuck selecting the line)
                    let count = match self.last_editor_click {
                        Some((t, p, c)) if t.elapsed() < EDITOR_CLICK_TTL && p == click => {
                            if c >= 3 { 1 } else { c + 1 }
                        }
                        _ => 1,
                    };
                    self.last_editor_click = Some((Instant::now(), click, count));
                    match count {
                        2 => {
                            if let Some((line, col)) = click {
                                self.buffer.select_word_at((col, line));
                            }
                        }
                        3 => {
                            if let Some((line, _)) = click {
                                self.buffer.select_line(line);
                            }
                        }
                        _ => {
                            if event.modifiers.contains(KeyModifiers::SHIFT) {
                                if !self.buffer.selecting {
                                    self.buffer.begin_selection();
                                }
                            } else {
                                self.buffer.clear_selection();
                            }
                            if let Some((line, col)) = click {
                                self.buffer.cursor = (col, line);
                            }
                        }
                    }
                    self.buffer.sync_parinfer_cursor();
                    self.ensure_cursor_visible();
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.in_sidebar(pos) {
                    if let Some(row) = self.sidebar_row_at(pos) {
                        self.sidebar.selected = self.sidebar.scroll + row;
                    }
                    // double-click opens the entry
                    let entry = self.sidebar.selected;
                    let double = self
                        .last_sidebar_click
                        .is_some_and(|(t, i)| i == entry && t.elapsed() < SIDEBAR_CLICK_TTL);
                    self.last_sidebar_click = Some((Instant::now(), entry));
                    if double {
                        self.open_selected();
                    }
                } else {
                    self.buffer.end_selection();
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.in_editor(pos) {
                    self.focus = Focus::Editor;
                    // a drag right after a double-/triple-click extends the
                    // selection word-/line-wise (like most editors); it also
                    // means small hand jitter while clicking snaps back to
                    // the same word instead of growing the selection
                    let click_count = self.last_editor_click.map_or(1, |(_, _, c)| c);
                    if click_count >= 2 {
                        if let Some((line, col)) = self.editor_cursor_at(pos) {
                            if click_count == 2 {
                                self.buffer.extend_selection_word_at((col, line));
                            } else {
                                self.buffer.extend_selection_line_at((col, line));
                            }
                            self.buffer.sync_parinfer_cursor();
                            self.ensure_cursor_visible();
                        }
                    } else {
                        if !self.buffer.selecting {
                            self.buffer.begin_selection();
                        }
                        if let Some((line, col)) = self.editor_cursor_at(pos) {
                            self.buffer.cursor = (col, line);
                            self.buffer.sync_parinfer_cursor();
                            self.ensure_cursor_visible();
                        }
                    }
                } else if self.in_sidebar(pos) {
                    self.focus = Focus::Sidebar;
                    if let Some(row) = self.sidebar_row_at(pos) {
                        self.sidebar.selected = self.sidebar.scroll + row;
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                if self.in_sidebar(pos) {
                    self.sidebar.move_selection(1);
                } else {
                    // Scrolling the editor changes only the viewport. The
                    // caret stays where it is, even when it moves off-screen.
                    self.scroll_editor(3);
                }
            }
            MouseEventKind::ScrollUp => {
                if self.in_sidebar(pos) {
                    self.sidebar.move_selection(-1);
                } else {
                    self.scroll_editor(-3);
                }
            }
            MouseEventKind::ScrollLeft if self.in_editor(pos) && !self.buffer.wrap => {
                // Horizontal scrolling only belongs to the editor. In
                // particular, do not let a horizontal wheel gesture over the
                // sidebar move the editor viewport.
                self.buffer.scroll.0 = self.buffer.scroll.0.saturating_sub(3);
            }
            MouseEventKind::ScrollRight if self.in_editor(pos) && !self.buffer.wrap => {
                self.buffer.scroll.0 += 3;
            }
            _ => {}
        }
    }

    pub(super) fn ensure_cursor_visible(&mut self) {
        let (w, h) = self.editor_text;
        self.buffer.ensure_visible(h as usize, w as usize);
    }

    /// Move the editor viewport without moving or reanchoring the cursor.
    pub(super) fn scroll_editor(&mut self, rows: isize) {
        self.buffer
            .scroll_vertical(rows, self.editor_text.1 as usize);
    }

    pub(super) fn in_topbar(&self, pos: (usize, usize)) -> bool {
        self.topbar_area
            .contains(Position::new(pos.0 as u16, pos.1 as u16))
    }

    /// The shortcut button under a mouse position, if any. Cell (0,0) is
    /// deliberately never a hit: the top bar starts one cell in, and
    /// terminals that report a synthetic (0,0) event when the mouse
    /// leaves the window must not light up (or trigger) anything.
    pub(super) fn topbar_action_at(&self, pos: (usize, usize)) -> Option<Shortcut> {
        if pos == (0, 0) {
            return None;
        }
        let p = Position::new(pos.0 as u16, pos.1 as u16);
        self.topbar_buttons
            .iter()
            .find(|(_, rect)| rect.contains(p))
            .map(|(action, _)| *action)
    }

    pub(super) fn in_sidebar(&self, pos: (usize, usize)) -> bool {
        self.sidebar_area
            .contains(Position::new(pos.0 as u16, pos.1 as u16))
    }

    pub(super) fn in_editor(&self, pos: (usize, usize)) -> bool {
        self.editor_area
            .contains(Position::new(pos.0 as u16, pos.1 as u16))
    }

    /// Whether a position is over rendered editor text rather than its border
    /// or line-number gutter. This prevents a Ctrl/Cmd-click on the gutter
    /// from opening a URL that happens to start at column zero.
    pub(super) fn in_editor_text(&self, pos: (usize, usize)) -> bool {
        let area = self.editor_area;
        let gutter_w = self.buffer.lines.len().to_string().len() + 1;
        let left = area.x as usize + 1 + gutter_w;
        let right = area.x as usize + area.width.saturating_sub(1) as usize;
        let top = area.y as usize + 1;
        let bottom = area.y as usize + area.height.saturating_sub(1) as usize;
        pos.0 >= left && pos.0 < right && pos.1 >= top && pos.1 < bottom
    }

    /// Open the link at a clicked editor position. The URL is owned by this
    /// point so showing a status message cannot keep a borrow of the buffer
    /// alive.
    pub(super) fn open_link_at(&mut self, click: Option<(usize, usize)>) -> bool {
        let Some((line, column)) = click else {
            return false;
        };
        let Some(url) = self
            .buffer
            .lines
            .get(line)
            .and_then(|text| link_at(text, column))
        else {
            return false;
        };

        match open_in_browser(&url) {
            Ok(()) => self.set_message(format!("opened {url}")),
            Err(error) => self.set_message(format!("could not open link: {error}")),
        }
        true
    }

    /// Whether a mouse event represents the platform's link-opening modifier.
    /// Cmd is reported as SUPER by terminals that support the kitty keyboard
    /// protocol, while Ctrl remains the portable fallback.
    pub(super) fn is_ctrl_or_cmd(modifiers: KeyModifiers) -> bool {
        modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
    }

    /// Row within the sidebar's visible entries for a mouse position.
    pub(super) fn sidebar_row_at(&self, pos: (usize, usize)) -> Option<usize> {
        let area = self.sidebar_area;
        let inner_y = area.y + 1;
        let inner_h = area.height.saturating_sub(2);
        if pos.1 < inner_y as usize || pos.1 >= (inner_y + inner_h) as usize {
            return None;
        }
        let row = pos.1 - inner_y as usize;
        if row >= self.sidebar.entries.len() {
            return None;
        }
        Some(row)
    }

    /// Buffer `(line, char)` position for a mouse position in the editor,
    /// clamped into the visible text area.
    pub(super) fn editor_cursor_at(&self, pos: (usize, usize)) -> Option<(usize, usize)> {
        let area = self.editor_area;
        let inner_x = area.x + 1;
        let inner_y = area.y + 1;
        let inner_w = area.width.saturating_sub(2);
        let inner_h = area.height.saturating_sub(2);
        if inner_w == 0 || inner_h == 0 || self.buffer.lines.is_empty() {
            return None;
        }
        let rel_y = pos
            .1
            .saturating_sub(inner_y as usize)
            .min(inner_h as usize - 1);
        let rel_x = pos.0.saturating_sub(inner_x as usize);
        // clicks in the line-number gutter land at column 0
        let gutter_w = self.buffer.lines.len().to_string().len() + 1;
        let col = rel_x.saturating_sub(gutter_w);
        if self.buffer.wrap {
            // the row clicked is a visual row: map it to its logical
            // line and chunk, then to a char index within the chunk
            let width = self.buffer.wrap_width.max(1);
            let vrow = (self.buffer.scroll.1 + rel_y).min(self.buffer.total_visual_rows() - 1);
            let (y, k) = self.buffer.vrow_position(vrow)?;
            let line = &self.buffer.lines[y];
            let (start, end) = visual_chunk(line, k, width);
            // Only the clicked visual row participates in horizontal hit
            // testing. Scanning the rest of the logical line would map a
            // click in the blank tail of a short wrapped row onto a later
            // row, which then corrupts vertical navigation.
            let chunk: String = line
                .chars()
                .skip(start)
                .take(end.saturating_sub(start))
                .collect();
            let x = start + char_at_col(&chunk, col).min(end.saturating_sub(start));
            return Some((y, x));
        }
        let y = (self.buffer.scroll.1 + rel_y).min(self.buffer.lines.len() - 1);
        let line = &self.buffer.lines[y];
        let visible: String = line.chars().skip(self.buffer.scroll.0).collect();
        let x = self.buffer.scroll.0 + char_at_col(&visible, col);
        Some((y, x))
    }
}
