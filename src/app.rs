//! Application state, key handling and rendering.

use std::io;
use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::buffer::Buffer;
use crate::clipboard::{Clipboard, SystemClipboard};
use crate::highlight::Highlighter;
use crate::sidebar::{Kind, Sidebar};

/// How long transient status messages stay visible.
const MESSAGE_TTL: Duration = Duration::from_secs(4);

/// Max interval between two clicks on the same sidebar entry for them to
/// count as a double-click (which opens the entry).
const SIDEBAR_CLICK_TTL: Duration = Duration::from_millis(400);

/// Max interval between consecutive editor clicks at the same position for
/// them to count as a double- (word) or triple-click (line). Deliberately
/// shorter than the OS double-click window: a single click followed by a
/// separate double-click (a very common habit) would otherwise drift into a
/// triple-click and select the whole line. Genuine double- and triple-clicks
/// are almost always much faster than this.
const EDITOR_CLICK_TTL: Duration = Duration::from_millis(250);

const SIDEBAR_WIDTH: u16 = 28;
const STATUS_HEIGHT: u16 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Sidebar,
    Editor,
}

/// A click on the editor: (time, buffer position, consecutive-click count).
/// The position is `None` when the click misses the text area.
type EditorClick = (Instant, Option<(usize, usize)>, u32);

pub struct App {
    pub buffer: Buffer,
    pub sidebar: Sidebar,
    pub focus: Focus,
    pub should_quit: bool,
    highlighter: Highlighter,
    pub clipboard: Box<dyn Clipboard>,
    /// Transient status message with expiry.
    message: Option<(String, Instant)>,
    /// Active "save as" input text, when the buffer has no file name.
    save_as_input: Option<String>,
    /// Set when Ctrl+Q is pressed with unsaved changes; second press quits.
    quit_armed: bool,
    /// Viewport sizes from the last draw, used for paging and scrolling.
    editor_text: (u16, u16),
    sidebar_height: u16,
    /// Widget areas from the last draw, used for mouse hit-testing.
    sidebar_area: Rect,
    editor_area: Rect,
    /// Last click on the sidebar, for double-click detection.
    last_sidebar_click: Option<(Instant, usize)>,
    /// Last click in the editor, for double-click (word) and triple-click
    /// (line) selection.
    last_editor_click: Option<EditorClick>,
}

impl App {
    pub fn new(dir: PathBuf, file: Option<PathBuf>) -> io::Result<Self> {
        let mut highlighter = Highlighter::new();
        highlighter.set_path(file.as_deref());
        let buffer = match &file {
            Some(path) => Buffer::from_path(path.clone())?,
            None => Buffer::empty(),
        };
        let focus = if buffer.path.is_some() {
            Focus::Editor
        } else {
            Focus::Sidebar
        };
        let mut sidebar = Sidebar::new(dir)?;
        if let Some(name) = buffer.path.as_ref().and_then(|p| p.file_name()) {
            sidebar.select_name(&name.to_string_lossy());
        }
        Ok(Self {
            buffer,
            sidebar,
            focus,
            should_quit: false,
            highlighter,
            clipboard: Box::new(SystemClipboard::new()),
            message: None,
            save_as_input: None,
            quit_armed: false,
            editor_text: (0, 0),
            sidebar_height: 0,
            sidebar_area: Rect::default(),
            editor_area: Rect::default(),
            last_sidebar_click: None,
            last_editor_click: None,
        })
    }

    fn set_message(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now() + MESSAGE_TTL));
    }

    // ---- key handling ------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) {
        // Global shortcuts work everywhere, even inside the save-as prompt.
        // On macOS, Cmd+key is reported as SUPER on terminals that speak the
        // kitty keyboard protocol; plain Ctrl+key works everywhere.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::SUPER);
        if ctrl {
            match key.code {
                KeyCode::Char('q') => {
                    if self.buffer.dirty && !self.quit_armed {
                        self.quit_armed = true;
                        self.set_message("unsaved changes — press Ctrl+Q again to quit anyway");
                    } else {
                        self.should_quit = true;
                    }
                    return;
                }
                KeyCode::Char('s') => {
                    self.save();
                    return;
                }
                KeyCode::Char('o') => {
                    if self.save_as_input.is_none() {
                        self.focus = match self.focus {
                            Focus::Sidebar => Focus::Editor,
                            Focus::Editor => Focus::Sidebar,
                        };
                    }
                    return;
                }
                KeyCode::Char('c') => {
                    if self.save_as_input.is_none() {
                        self.copy_selection();
                    }
                    return;
                }
                KeyCode::Char('x') => {
                    if self.save_as_input.is_none() {
                        self.cut_selection();
                    }
                    return;
                }
                KeyCode::Char('v') => {
                    self.paste_clipboard();
                    return;
                }
                KeyCode::Char('a') => {
                    if self.save_as_input.is_none() {
                        self.buffer.select_all();
                    }
                    return;
                }
                // Ctrl+Z undoes, Ctrl+Shift+Z redoes (CapsLock typos land
                // on redo, a harmless no-op without history). The shifted
                // letter may arrive as 'Z' or as 'z'+Shift depending on
                // the terminal, so accept both.
                KeyCode::Char('z') | KeyCode::Char('Z') => {
                    if self.save_as_input.is_none() {
                        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                        if key.code == KeyCode::Char('z') && !shift {
                            self.undo();
                        } else {
                            self.redo();
                        }
                    }
                    return;
                }
                _ => return,
            }
        }

        if let Some(input) = self.save_as_input.as_mut() {
            match key.code {
                KeyCode::Esc => self.save_as_input = None,
                KeyCode::Enter => self.confirm_save_as(),
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => input.push(c),
                _ => {}
            }
            return;
        }

        match self.focus {
            Focus::Sidebar => self.handle_sidebar_key(key),
            Focus::Editor => self.handle_editor_key(key),
        }
    }

    fn handle_sidebar_key(&mut self, key: KeyEvent) {
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
            KeyCode::Enter => self.open_selected(),
            KeyCode::Backspace => match self.sidebar.ascend() {
                Ok(()) => {}
                Err(e) => self.set_message(format!("cannot go up: {e}")),
            },
            _ => {}
        }
    }

    fn handle_editor_key(&mut self, key: KeyEvent) {
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
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                if !c.is_control() {
                    self.buffer.insert_char(c);
                }
            }
            KeyCode::Enter => self.buffer.newline(),
            KeyCode::Tab => self.buffer.insert_text("    "),
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
        // invalidate the highlight cache at the first changed line
        if let Some(line) = self.buffer.last_edit_line.take() {
            self.highlighter.invalidate_from(line);
        }
        self.buffer.ensure_visible(h as usize, w as usize);
    }

    // ---- undo / redo ------------------------------------------------------

    fn undo(&mut self) {
        if self.buffer.undo() {
            self.after_undo_redo();
        }
    }

    fn redo(&mut self) {
        if self.buffer.redo() {
            self.after_undo_redo();
        }
    }

    /// Re-highlight the changed lines, keep the restored cursor visible,
    /// and re-arm the quit guard (the buffer changed again).
    fn after_undo_redo(&mut self) {
        if let Some(line) = self.buffer.last_edit_line.take() {
            self.highlighter.invalidate_from(line);
        }
        let (w, h) = self.editor_text;
        self.buffer.ensure_visible(h as usize, w as usize);
        self.quit_armed = false;
    }

    // ---- clipboard ---------------------------------------------------------

    fn copy_selection(&mut self) {
        let Some(text) = self.buffer.selected_text() else {
            return;
        };
        self.clipboard.set_text(&text);
        self.set_message("copied");
    }

    fn cut_selection(&mut self) {
        if !self.buffer.has_selection() {
            return;
        }
        let text = self.buffer.selected_text().unwrap_or_default();
        self.clipboard.set_text(&text);
        self.buffer.delete_selection();
        if let Some(line) = self.buffer.last_edit_line.take() {
            self.highlighter.invalidate_from(line);
        }
        self.set_message("cut");
    }

    fn paste_clipboard(&mut self) {
        let Some(text) = self.clipboard.get_text() else {
            self.set_message("clipboard unavailable");
            return;
        };
        if text.is_empty() {
            return;
        }
        if let Some(input) = self.save_as_input.as_mut() {
            input.push_str(&text);
            return;
        }
        self.paste_text(text);
    }

    /// Insert pasted text (from Ctrl+V or bracketed paste) into the buffer.
    pub fn paste_text(&mut self, text: String) {
        self.focus = Focus::Editor;
        self.buffer.insert_multiline(&text);
        if let Some(line) = self.buffer.last_edit_line.take() {
            self.highlighter.invalidate_from(line);
        }
        self.quit_armed = false;
    }

    // ---- mouse -------------------------------------------------------------

    pub fn handle_mouse(&mut self, event: MouseEvent) {
        let pos = (event.column as usize, event.row as usize);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.quit_armed = false;
                if self.in_sidebar(pos) {
                    self.focus = Focus::Sidebar;
                    if let Some(row) = self.sidebar_row_at(pos) {
                        self.sidebar.selected = self.sidebar.scroll + row;
                    }
                } else if self.in_editor(pos) {
                    self.focus = Focus::Editor;
                    let click = self.editor_cursor_at(pos);
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
                            self.ensure_cursor_visible();
                        }
                    } else {
                        if !self.buffer.selecting {
                            self.buffer.begin_selection();
                        }
                        if let Some((line, col)) = self.editor_cursor_at(pos) {
                            self.buffer.cursor = (col, line);
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
                    for _ in 0..3 {
                        self.buffer.move_down();
                    }
                    self.ensure_cursor_visible();
                }
            }
            MouseEventKind::ScrollUp => {
                if self.in_sidebar(pos) {
                    self.sidebar.move_selection(-1);
                } else {
                    for _ in 0..3 {
                        self.buffer.move_up();
                    }
                    self.ensure_cursor_visible();
                }
            }
            MouseEventKind::ScrollLeft => {
                self.buffer.scroll.0 = self.buffer.scroll.0.saturating_sub(3);
            }
            MouseEventKind::ScrollRight => {
                self.buffer.scroll.0 += 3;
            }
            _ => {}
        }
    }

    fn ensure_cursor_visible(&mut self) {
        let (w, h) = self.editor_text;
        self.buffer.ensure_visible(h as usize, w as usize);
    }

    fn in_sidebar(&self, pos: (usize, usize)) -> bool {
        self.sidebar_area
            .contains(Position::new(pos.0 as u16, pos.1 as u16))
    }

    fn in_editor(&self, pos: (usize, usize)) -> bool {
        self.editor_area
            .contains(Position::new(pos.0 as u16, pos.1 as u16))
    }

    /// Row within the sidebar's visible entries for a mouse position.
    fn sidebar_row_at(&self, pos: (usize, usize)) -> Option<usize> {
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
    fn editor_cursor_at(&self, pos: (usize, usize)) -> Option<(usize, usize)> {
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
        let y = (self.buffer.scroll.1 + rel_y).min(self.buffer.lines.len() - 1);
        // clicks in the line-number gutter land at column 0
        let gutter_w = self.buffer.lines.len().to_string().len() + 1;
        let col = rel_x.saturating_sub(gutter_w);
        let line = &self.buffer.lines[y];
        let visible: String = line.chars().skip(self.buffer.scroll.0).collect();
        let x = self.buffer.scroll.0 + char_at_col(&visible, col);
        Some((y, x))
    }

    // ---- file operations ---------------------------------------------------

    fn save(&mut self) {
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
        }
    }

    fn confirm_save_as(&mut self) {
        let Some(input) = self.save_as_input.take() else {
            return;
        };
        let input = input.trim().to_string();
        if input.is_empty() {
            return;
        }
        let path = PathBuf::from(&input);
        if path.is_dir() {
            self.set_message(format!("{input} is a directory"));
            return;
        }
        self.buffer.path = Some(path.clone());
        self.highlighter.set_path(Some(&path));
        match self.buffer.save() {
            Ok(()) => {
                self.quit_armed = false;
                // refresh the sidebar so the new file shows up
                if let Err(e) = self.sidebar.reload() {
                    self.set_message(format!(
                        "saved {}, but sidebar refresh failed: {e}",
                        path.display()
                    ));
                    return;
                }
                self.set_message(format!("saved {}", path.display()));
            }
            Err(e) => self.set_message(format!("save failed: {e}")),
        }
    }

    fn open_selected(&mut self) {
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
        match Buffer::from_path(path.clone()) {
            Ok(buffer) => {
                self.buffer = buffer;
                self.highlighter.set_path(Some(&path));
                self.quit_armed = false;
                self.focus = Focus::Editor;
                self.set_message(format!("opened {}", path.display()));
            }
            Err(e) => self.set_message(format!("cannot open {}: {e}", path.display())),
        }
    }

    // ---- drawing -----------------------------------------------------------

    pub fn draw(&mut self, frame: &mut Frame) {
        let [main, status_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(STATUS_HEIGHT)])
                .areas(frame.area());
        let [side_area, edit_area] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(main);
        self.sidebar_area = side_area;
        self.editor_area = edit_area;

        self.draw_sidebar(frame, side_area);
        self.draw_editor(frame, edit_area);
        self.draw_status(frame, status_area);
    }

    fn draw_sidebar(&mut self, frame: &mut Frame, area: Rect) {
        self.sidebar_height = area.height;
        let inner_h = area.height.saturating_sub(2) as usize;
        self.sidebar.ensure_selected_visible(inner_h);

        let mut rows = Vec::with_capacity(self.sidebar.entries.len());
        for (i, entry) in self.sidebar.entries.iter().enumerate() {
            let display = match entry.kind {
                Kind::Parent | Kind::Dir => format!("{}/", entry.name),
                Kind::File => entry.name.clone(),
            };
            let mut style = match entry.kind {
                Kind::Parent => Style::default().fg(Color::DarkGray),
                Kind::Dir => Style::default().fg(Color::Cyan),
                Kind::File => Style::default().fg(Color::Gray),
            };
            if entry.is_hidden() {
                style = style.add_modifier(Modifier::DIM);
            }
            let selected = i == self.sidebar.selected;
            if selected {
                style = style.bg(Color::DarkGray);
            }
            let marker = if selected { "▶ " } else { "  " };
            rows.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Yellow)),
                Span::styled(display, style),
            ]));
        }
        if rows.is_empty() {
            rows.push(Line::from(Span::styled(
                "(empty)",
                Style::default().fg(Color::DarkGray),
            )));
        }

        let title = truncate(&self.sidebar.dir.display().to_string(), area.width as usize);
        let block = Block::bordered()
            .title(Span::styled(title, Style::default().fg(Color::Cyan)))
            .title_style(Style::default().add_modifier(Modifier::BOLD))
            .border_style(if self.focus == Focus::Sidebar {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            });
        let paragraph = Paragraph::new(rows)
            .block(block)
            .scroll((self.sidebar.scroll as u16, 0));
        frame.render_widget(paragraph, area);

        // Put the terminal cursor on the selected row while the sidebar has
        // focus. The editor draws its own colored caret.
        if self.focus == Focus::Sidebar {
            let row = (self.sidebar.selected - self.sidebar.scroll) as u16;
            frame.set_cursor_position(Position::new(area.x + 1, area.y + 1 + row));
        }
    }

    fn draw_editor(&mut self, frame: &mut Frame, area: Rect) {
        let title = self
            .buffer
            .path
            .as_ref()
            .map_or_else(|| "untitled".to_string(), |p| p.display().to_string());
        let title = truncate(&title, area.width as usize);

        let block = Block::bordered()
            .title(Span::styled(title, Style::default().fg(Color::Green)))
            .title_style(Style::default().add_modifier(Modifier::BOLD))
            .border_style(if self.focus == Focus::Editor {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            });
        let inner = block.inner(area);

        let gutter_w = self.buffer.lines.len().to_string().len() + 1;
        let text_w = inner.width.saturating_sub(gutter_w as u16) as usize;
        let text_h = inner.height as usize;
        self.editor_text = (text_w as u16, text_h as u16);
        self.buffer.ensure_visible(text_h, text_w);

        let start = self.buffer.scroll.1;
        let end = (start + text_h).min(self.buffer.lines.len());
        let mut rows: Vec<Line> = Vec::with_capacity(end.saturating_sub(start));
        let mut caret_style = None;
        for y in start..end {
            let num = Span::styled(
                format!("{:>width$} ", y + 1, width = gutter_w - 1),
                Style::default().fg(Color::DarkGray),
            );
            let ops = self.highlighter.highlight_line(&self.buffer.lines, y);
            let line = &self.buffer.lines[y];
            if self.focus == Focus::Editor
                && self.save_as_input.is_none()
                && self.buffer.cursor.1 == y
                && self.buffer.cursor.0 >= self.buffer.scroll.0
                && self.buffer.cursor.0 < self.buffer.scroll.0 + text_w
            {
                let cursor_byte = char_index_to_byte(line, self.buffer.cursor.0);
                caret_style = Some(
                    ops.iter()
                        .find(|(_, range)| range.contains(&cursor_byte))
                        .and_then(|(style, _)| *style)
                        .unwrap_or_default(),
                );
            }
            // selection overlap on this line, in byte offsets
            let sel = self
                .buffer
                .selection_on_line(y)
                .map(|(a, b)| (char_index_to_byte(line, a), char_index_to_byte(line, b)));
            let mut spans = vec![num];
            spans.extend(clip_ops(line, &ops, self.buffer.scroll.0, text_w, sel));
            rows.push(Line::from(spans));
        }
        if rows.is_empty() {
            rows.push(Line::from(Span::styled(
                "(empty)",
                Style::default().fg(Color::DarkGray),
            )));
        }

        let paragraph = Paragraph::new(rows).block(block);
        frame.render_widget(paragraph, area);

        if self.focus == Focus::Editor && self.save_as_input.is_none() {
            // Ratatui can position the terminal cursor, but cannot give it a
            // color. Render a block caret ourselves so it remains distinct
            // from the reversed selection style (and leave the native cursor
            // hidden).
            let cx = area.x + 1 + gutter_w as u16 + self.buffer.cursor_col() as u16;
            let cy = area.y + 1 + (self.buffer.cursor.1 - self.buffer.scroll.1) as u16;
            let inner_right = area.x + area.width.saturating_sub(1);
            let inner_bottom = area.y + area.height.saturating_sub(1);
            if cx < inner_right && cy < inner_bottom {
                let symbol = self
                    .buffer
                    .lines
                    .get(self.buffer.cursor.1)
                    .and_then(|line| line.chars().nth(self.buffer.cursor.0))
                    .map_or_else(|| " ".to_string(), |c| c.to_string());
                let caret = Paragraph::new(Span::styled(
                    symbol,
                    caret_style.unwrap_or_default().bg(Color::Yellow),
                ));
                frame.render_widget(caret, Rect::new(cx, cy, 1, 1));
            }
        }
    }

    fn draw_status(&mut self, frame: &mut Frame, area: Rect) {
        // "save as" prompt replaces the status bar content
        if let Some(input) = &self.save_as_input {
            let prompt = "save as: ";
            let prompt_w = prompt.width() as u16;
            let input_w = input.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>() as u16;
            let paragraph = Paragraph::new(Line::from(vec![
                Span::styled(
                    prompt,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(input.clone(), Style::default().fg(Color::White)),
            ]))
            .style(Style::default().bg(Color::Rgb(30, 30, 30)));
            frame.render_widget(paragraph, area);
            frame.set_cursor_position(Position::new(area.x + prompt_w + input_w, area.y));
            return;
        }

        // right: position + help
        let (x, y) = self.buffer.cursor;
        let right = format!(
            "{}:{}   Ctrl+O switch · Ctrl+S save · Ctrl+Z undo · Ctrl+Shift+Z redo · Ctrl+C/X/V copy/cut/paste · Ctrl+Q quit",
            y + 1,
            x + 1
        );
        let right_width = right.width() as u16;

        let [left_area, right_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(area);
        let base = Style::default().bg(Color::Rgb(30, 30, 30)).fg(Color::Gray);

        // left: focus + file + modified state, or a transient message
        let (left_spans, left_style): (Vec<Span>, Style) =
            if let Some((msg, expiry)) = &self.message {
                if *expiry > Instant::now() {
                    // keep the modified indicator visible even while a message
                    // is showing
                    let mut spans = vec![Span::styled(
                        msg.clone(),
                        Style::default().fg(Color::Yellow),
                    )];
                    if self.buffer.dirty {
                        spans.push(Span::styled(
                            " ● modified",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                    (spans, Style::default())
                } else {
                    self.message = None;
                    self.status_left(left_area.width)
                }
            } else {
                self.status_left(left_area.width)
            };
        frame.render_widget(
            Paragraph::new(Line::from(left_spans)).style(base.patch(left_style)),
            left_area,
        );
        frame.render_widget(
            Paragraph::new(Line::from(right))
                .style(base)
                .alignment(ratatui::layout::Alignment::Right),
            right_area,
        );
    }

    /// The normal (non-message) left side of the status bar. Truncates the
    /// path so the focus tag and the modified indicator always stay visible.
    fn status_left(&self, width: u16) -> (Vec<Span<'static>>, Style) {
        let (tag, tag_color) = match self.focus {
            Focus::Sidebar => ("SIDEBAR", Color::Cyan),
            Focus::Editor => ("EDITOR", Color::Green),
        };
        let path = self
            .buffer
            .path
            .as_ref()
            .map_or_else(|| "untitled".to_string(), |p| p.display().to_string());
        let dirty = if self.buffer.dirty {
            "● modified"
        } else {
            "○ saved"
        };
        let syntax = if self.highlighter.syntax_name() != "Plain Text" {
            format!("[{}]", self.highlighter.syntax_name())
        } else {
            String::new()
        };
        let path_max = width
            .saturating_sub(tag.width() as u16 + dirty.width() as u16 + syntax.width() as u16 + 2);
        let path = truncate(&path, path_max as usize);
        (
            vec![
                Span::styled(
                    tag,
                    Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" {path} ")),
                Span::styled(syntax, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    dirty,
                    if self.buffer.dirty {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Green)
                    },
                ),
            ],
            Style::default(),
        )
    }
}

/// Shorten `s` to at most `max` chars, keeping the end and prefixing "…".
fn truncate(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let keep = max - 1;
    let mut out: String = s.chars().skip(count - keep).collect();
    out.insert(0, '…');
    out
}

/// Byte index of the `idx`-th char in `s` (or `s.len()` if past the end).
fn char_index_to_byte(s: &str, idx: usize) -> usize {
    s.char_indices()
        .nth(idx)
        .map(|(byte, _)| byte)
        .unwrap_or(s.len())
}

/// Char index at (or just past) terminal column `col`, accounting for wide
/// characters: the cursor lands between chars at the clicked cell boundary.
fn char_at_col(s: &str, col: usize) -> usize {
    let mut width = 0;
    for (i, c) in s.chars().enumerate() {
        if width >= col {
            return i;
        }
        width += c.width().unwrap_or(0);
    }
    s.chars().count()
}

/// Snap a byte offset up to the next char boundary.
fn snap_char_up(s: &str, mut b: usize) -> usize {
    while b < s.len() && !s.is_char_boundary(b) {
        b += 1;
    }
    b
}

/// Snap a byte offset down to the previous char boundary.
fn snap_char_down(s: &str, mut b: usize) -> usize {
    while b > 0 && !s.is_char_boundary(b) {
        b -= 1;
    }
    b
}

/// Clip styled byte-ranges from the highlighter to the visible char slice
/// `[start_char, start_char + width)`, producing the `Span`s to render.
/// `None` styles render as plain text (terminal default colors); spans
/// overlapping `sel` (byte range on this line) are shown reversed.
fn clip_ops<'a>(
    line: &'a str,
    ops: &[(Option<Style>, Range<usize>)],
    start_char: usize,
    width: usize,
    sel: Option<(usize, usize)>,
) -> Vec<Span<'a>> {
    let start_byte = char_index_to_byte(line, start_char);
    let end_byte = char_index_to_byte(line, start_char + width);
    let mut out = Vec::new();
    for (style, range) in ops {
        let a = range.start.max(start_byte);
        let b = range.end.min(end_byte);
        if a >= b {
            continue;
        }
        // syntect ranges are char-aligned, but be safe
        let a = snap_char_up(line, a);
        let b = snap_char_down(line, b);
        if a >= b {
            continue;
        }
        let mut push_span = |a: usize, b: usize, selected: bool| {
            if a >= b {
                return;
            }
            let text = &line[a..b];
            match (style, selected) {
                (Some(style), false) => out.push(Span::styled(text, *style)),
                (Some(style), true) => {
                    out.push(Span::styled(text, style.add_modifier(Modifier::REVERSED)))
                }
                (None, false) => out.push(Span::raw(text)),
                (None, true) => out.push(Span::styled(
                    text,
                    Style::default().add_modifier(Modifier::REVERSED),
                )),
            }
        };

        if let Some((sa, sb)) = sel.filter(|(sa, sb)| a < *sb && b > *sa) {
            let selected_start = a.max(sa);
            let selected_end = b.min(sb);
            push_span(a, selected_start, false);
            push_span(selected_start, selected_end, true);
            push_span(selected_end, b, false);
        } else {
            push_span(a, b, false);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};

    #[derive(Default)]
    struct FakeClipboard {
        text: String,
    }

    impl Clipboard for FakeClipboard {
        fn get_text(&mut self) -> Option<String> {
            Some(self.text.clone())
        }

        fn set_text(&mut self, text: &str) {
            self.text = text.to_string();
        }
    }

    fn with_fake_clipboard(mut app: App) -> App {
        app.clipboard = Box::new(FakeClipboard::default());
        app
    }
    use std::fs;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn cmd(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::SUPER)
    }

    fn shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn char_key(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!("ratatata-app-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn opening_a_file_arg_focuses_editor() {
        let dir = scratch("filearg");
        let file = dir.join("a.txt");
        fs::write(&file, "hello\nworld\n").unwrap();
        let app = App::new(dir, Some(file)).unwrap();
        assert_eq!(app.focus, Focus::Editor);
        assert_eq!(app.buffer.lines, vec!["hello", "world", ""]);
        assert!(!app.buffer.dirty);
    }

    #[test]
    fn directory_arg_focuses_sidebar() {
        let dir = scratch("dira");
        let app = App::new(dir, None).unwrap();
        assert_eq!(app.focus, Focus::Sidebar);
        assert!(app.buffer.path.is_none());
    }

    #[test]
    fn ctrl_o_toggles_focus() {
        let dir = scratch("toggle");
        let mut app = App::new(dir, None).unwrap();
        assert_eq!(app.focus, Focus::Sidebar);
        app.handle_key(ctrl('o'));
        assert_eq!(app.focus, Focus::Editor);
        app.handle_key(ctrl('o'));
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[test]
    fn save_as_flow_creates_file_and_updates_sidebar() {
        let dir = scratch("saveas");
        let mut app = App::new(dir.clone(), None).unwrap();

        app.handle_key(ctrl('o')); // focus editor
        for c in "hello".chars() {
            app.handle_key(char_key(c));
        }
        assert!(app.buffer.dirty);

        // Ctrl+S with no file name enters the save-as prompt
        app.handle_key(ctrl('s'));
        assert!(app.save_as_input.is_some());

        // typing while the prompt is active goes into the prompt, not the buffer
        let save_path = dir.join("out.txt");
        for c in save_path.display().to_string().chars() {
            app.handle_key(char_key(c));
        }
        assert_eq!(
            app.save_as_input.as_deref(),
            Some(save_path.to_str().unwrap())
        );
        assert_eq!(app.buffer.lines, vec!["hello"]);

        app.handle_key(key(KeyCode::Enter));
        assert!(app.save_as_input.is_none());
        assert!(!app.buffer.dirty);
        assert_eq!(fs::read_to_string(&save_path).unwrap(), "hello");
        // the sidebar was refreshed and now lists the new file
        assert!(app.sidebar.entries.iter().any(|e| e.name == "out.txt"));
    }

    #[test]
    fn escape_cancels_save_as() {
        let dir = scratch("cancel");
        let mut app = App::new(dir, None).unwrap();
        app.handle_key(ctrl('o'));
        app.handle_key(ctrl('s'));
        app.handle_key(char_key('x'));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.save_as_input.is_none());
        assert!(app.buffer.path.is_none());
    }

    #[test]
    fn sidebar_enter_opens_file_and_switches_focus() {
        let dir = scratch("open");
        fs::write(dir.join("b.txt"), "beta").unwrap();
        let mut app = App::new(dir, None).unwrap();
        app.sidebar.select_name("b.txt");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Editor);
        assert_eq!(app.buffer.lines, vec!["beta"]);
        assert_eq!(
            app.buffer.path.as_ref().unwrap().file_name().unwrap(),
            "b.txt"
        );
    }

    #[test]
    fn dirty_buffer_blocks_opening_another_file() {
        let dir = scratch("dirtyblock");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        fs::write(dir.join("b.txt"), "beta").unwrap();
        let mut app = App::new(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        app.handle_key(char_key('X')); // make dirty
        assert!(app.buffer.dirty);

        app.sidebar.select_name("b.txt");
        app.handle_key(ctrl('o')); // sidebar
        app.handle_key(key(KeyCode::Enter));

        // still on a.txt with the edit intact (cursor starts at 0,0)
        assert_eq!(app.buffer.lines, vec!["Xalpha"]);
        assert_eq!(
            app.buffer.path.as_ref().unwrap().file_name().unwrap(),
            "a.txt"
        );
        assert!(app.message.is_some());
    }

    #[test]
    fn ctrl_s_saves_and_clears_dirty() {
        let dir = scratch("save");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha").unwrap();
        let mut app = App::new(dir.clone(), Some(file)).unwrap();
        app.handle_key(char_key('Z'));
        assert!(app.buffer.dirty);
        app.handle_key(ctrl('s'));
        assert!(!app.buffer.dirty);
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "Zalpha");
    }

    #[test]
    fn ctrl_q_requires_second_press_when_dirty() {
        let dir = scratch("quit");
        let mut app = App::new(dir, None).unwrap();
        app.handle_key(ctrl('o'));
        app.handle_key(char_key('x'));
        app.handle_key(ctrl('q'));
        assert!(!app.should_quit); // blocked, message shown
        app.handle_key(ctrl('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_q_quits_immediately_when_clean() {
        let dir = scratch("quit2");
        let mut app = App::new(dir, None).unwrap();
        app.handle_key(ctrl('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn sidebar_backspace_ascends() {
        let dir = scratch("ascend");
        fs::create_dir(dir.join("sub")).unwrap();
        let mut app = App::new(dir.clone(), None).unwrap();
        app.sidebar.select_name("sub");
        app.handle_key(key(KeyCode::Enter)); // descend
        assert_eq!(app.sidebar.dir, dir.join("sub"));
        app.handle_key(key(KeyCode::Backspace)); // ascend
        assert_eq!(app.sidebar.dir, dir);
    }

    // ---- headless rendering via ratatui's TestBackend --------------------

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(app: &mut App) -> Vec<String> {
        let backend = TestBackend::new(140, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(140)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect()
    }

    fn row_contains(rows: &[String], needle: &str) -> bool {
        rows.iter().any(|r| r.contains(needle))
    }

    #[test]
    fn renders_sidebar_editor_and_status() {
        let dir = scratch("render1");
        fs::create_dir(dir.join("docs")).unwrap();
        fs::write(dir.join("notes.txt"), "hello world").unwrap();
        let mut app = App::new(dir.clone(), None).unwrap();

        let rows = render(&mut app);
        // sidebar: dirs and files listed, selected row marked
        assert!(row_contains(&rows, "docs/"));
        assert!(row_contains(&rows, "notes.txt"));
        assert!(row_contains(&rows, "▶"));
        // editor: untitled block, empty buffer with line number 1
        assert!(row_contains(&rows, "untitled"));
        assert!(row_contains(&rows, "1 "));
        // status bar: untitled + saved indicator + focus tag + help
        assert!(row_contains(&rows, "SIDEBAR"));
        assert!(row_contains(&rows, "○ saved"));
        assert!(row_contains(&rows, "Ctrl+S save"));
        assert!(row_contains(&rows, "1:1"));
    }

    #[test]
    fn renders_file_content_and_modified_state() {
        let dir = scratch("render2");
        let file = dir.join("notes.txt");
        fs::write(&file, "hello world").unwrap();
        let mut app = App::new(dir.clone(), Some(file)).unwrap();

        // clean state shows the content and the saved marker
        let rows = render(&mut app);
        assert!(row_contains(&rows, "hello world"));
        assert!(row_contains(&rows, "EDITOR"));
        assert!(row_contains(&rows, "○ saved"));

        // type something -> modified marker appears
        app.handle_key(char_key('!'));
        let rows = render(&mut app);
        assert!(row_contains(&rows, "● modified"));

        // save -> message shown, and the marker returns once it expires
        app.handle_key(ctrl('s'));
        let rows = render(&mut app);
        assert!(row_contains(&rows, "saved "));
        app.message = None; // simulate the message expiring
        let rows = render(&mut app);
        assert!(row_contains(&rows, "○ saved"));
    }

    fn render_buffer(app: &mut App) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(140, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn renders_syntax_highlighted_code() {
        let dir = scratch("hlrender");
        let file = dir.join("code.rs");
        fs::write(&file, "fn main() {\n    let msg = \"hi\";\n}\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);
        // "fn" keyword: purple; "main" function name: blue-gray
        // (colors probed from the base16-ocean.dark theme)
        assert_eq!(buf.cell((31, 1)).unwrap().symbol(), "f");
        assert_eq!(
            buf.cell((31, 1)).unwrap().style().fg,
            Some(Color::Rgb(180, 142, 173))
        );
        assert_eq!(buf.cell((34, 1)).unwrap().symbol(), "m");
        assert_eq!(
            buf.cell((34, 1)).unwrap().style().fg,
            Some(Color::Rgb(143, 161, 179))
        );
        // string content "hi": green
        assert_eq!(buf.cell((46, 2)).unwrap().symbol(), "h");
        assert_eq!(
            buf.cell((46, 2)).unwrap().style().fg,
            Some(Color::Rgb(163, 190, 140))
        );
        // punctuation stays uncolored
        assert_eq!(buf.cell((31, 3)).unwrap().symbol(), "}");
        assert_eq!(buf.cell((31, 3)).unwrap().style().fg, Some(Color::Reset));
    }

    #[test]
    fn caret_has_distinct_rendered_style() {
        let dir = scratch("caretstyle");
        let file = dir.join("notes.txt");
        fs::write(&file, "test\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);
        let caret = buf.cell((31, 1)).unwrap();
        assert_eq!(caret.symbol(), "t");
        assert_eq!(caret.style().bg, Some(Color::Yellow));
        assert!(!caret.style().add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn plain_text_files_render_uncolored() {
        let dir = scratch("hlplain");
        let file = dir.join("notes.txt");
        fs::write(&file, "just some words\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);
        for x in 31..99 {
            // exclude the yellow focus border at x=99
            let cell = buf.cell((x, 1)).unwrap();
            if cell.symbol().is_empty() || cell.symbol() == " " {
                continue;
            }
            assert_eq!(cell.style().fg, Some(Color::Reset), "col {x}");
        }
    }

    #[test]
    fn editing_rehighlights_immediately() {
        let dir = scratch("hlrehighlight");
        let file = dir.join("code.rs");
        fs::write(&file, "fn main() {\n}\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);
        assert_ne!(buf.cell((31, 1)).unwrap().style().fg, Some(Color::Reset));
        // typing 'x' in front of "fn" must immediately re-highlight:
        // "xfn" is no longer a keyword
        app.handle_key(char_key('x'));
        let buf = render_buffer(&mut app);
        assert_eq!(buf.cell((31, 1)).unwrap().symbol(), "x");
        assert_eq!(buf.cell((31, 1)).unwrap().style().fg, Some(Color::Reset));
        assert_eq!(buf.cell((32, 1)).unwrap().symbol(), "f");
    }

    #[test]
    fn status_bar_shows_detected_syntax() {
        let dir = scratch("hlsyntax");
        let file = dir.join("code.rs");
        fs::write(&file, "fn main() {}\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        let rows = render(&mut app);
        assert!(row_contains(&rows, "[Rust]"));

        let dir = scratch("hlsyntax2");
        let file = dir.join("notes.txt");
        fs::write(&file, "hello\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        let rows = render(&mut app);
        assert!(!row_contains(&rows, "[Plain Text]"));
    }

    #[test]
    fn renders_save_as_prompt() {
        let dir = scratch("render3");
        let mut app = App::new(dir, None).unwrap();
        app.handle_key(ctrl('o'));
        app.handle_key(char_key('x'));
        app.handle_key(ctrl('s'));
        let rows = render(&mut app);
        assert!(row_contains(&rows, "save as:"));
    }

    #[test]
    fn renders_long_content_with_scrolling() {
        let dir = scratch("render4");
        let file = dir.join("long.txt");
        let content: String = (0..200).map(|i| format!("line {i:03}\n")).collect();
        fs::write(&file, content).unwrap();
        let mut app = App::new(dir.clone(), Some(file)).unwrap();

        // cursor at the end of the buffer; drawing must scroll it into view
        // without panicking
        app.buffer.cursor = (200, 199);
        let rows = render(&mut app);
        assert!(row_contains(&rows, "line 199"));
        assert!(!row_contains(&rows, "line 000"));
    }

    // ---- mouse -------------------------------------------------------------

    #[test]
    fn mouse_click_positions_cursor_and_focuses_editor() {
        let dir = scratch("mclick");
        let file = dir.join("code.rs");
        fs::write(&file, "fn main() {\n    let x = 1;\n}\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        render_buffer(&mut app); // sets widget areas

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 35, 2));
        assert_eq!(app.focus, Focus::Editor);
        // terminal col 35 = text col 4 on line 2 (index 1)
        assert_eq!(app.buffer.cursor, (4, 1));

        // clicking in the gutter lands at column 0
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 29, 3));
        assert_eq!(app.buffer.cursor, (0, 2));
    }

    #[test]
    fn mouse_drag_selects_text() {
        let dir = scratch("mdrag");
        let file = dir.join("code.rs");
        fs::write(&file, "fn main() {\n}\n").unwrap();
        let mut app = with_fake_clipboard(App::new(dir, Some(file)).unwrap());
        render_buffer(&mut app);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 1));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 33, 1));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 33, 1));
        assert_eq!(app.buffer.selected_text().as_deref(), Some("fn"));

        // Ctrl+C copies the selection to the clipboard
        app.handle_key(ctrl('c'));
        assert_eq!(app.clipboard.get_text().as_deref(), Some("fn"));
    }

    #[test]
    fn shift_click_extends_selection() {
        let dir = scratch("mshift");
        let file = dir.join("code.rs");
        fs::write(&file, "fn main() {\n}\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 33, 1));
        let shift_click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 36,
            row: 1,
            modifiers: KeyModifiers::SHIFT,
        };
        app.handle_mouse(shift_click);
        assert_eq!(app.buffer.selection_range(), Some(((2, 0), (5, 0))));
    }

    #[test]
    fn double_click_selects_word() {
        let dir = scratch("mdblword");
        let file = dir.join("a.txt");
        fs::write(&file, "hello brave world\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        // terminal col 37 = char 6 = 'b' of "brave"
        let click = |kind| mouse(kind, 37, 1);
        // first click just places the cursor
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        assert!(!app.buffer.has_selection());
        assert_eq!(app.buffer.cursor, (6, 0));

        // second click on the same spot within the TTL selects the word
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        assert_eq!(app.buffer.selected_text().as_deref(), Some("brave"));
        // selecting is finished (mouse released)
        assert!(!app.buffer.selecting);
    }

    #[test]
    fn triple_click_selects_line() {
        let dir = scratch("mtriple");
        let file = dir.join("a.txt");
        fs::write(&file, "one two three\nfour five\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        let click = |kind| mouse(kind, 31, 1); // char 0 of line 0
        for _ in 0..3 {
            app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
            app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        }
        assert_eq!(app.buffer.selected_text().as_deref(), Some("one two three"));
        assert_eq!(app.buffer.selection_range(), Some(((0, 0), (13, 0))));

        // a fourth rapid click starts a fresh sequence: it's a plain click
        // and clears the line selection (extra clicks never get stuck)
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        assert!(!app.buffer.has_selection());
    }

    #[test]
    fn single_click_then_double_click_selects_word_not_line() {
        let dir = scratch("mdblafterclick");
        let file = dir.join("a.txt");
        fs::write(&file, "hello brave world\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        let click = |kind| mouse(kind, 37, 1); // char 6 = 'b' of "brave"
        // the user clicks once to place the cursor...
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        // ...hesitates longer than the editor click window...
        app.last_editor_click =
            Some((Instant::now() - Duration::from_millis(300), Some((6, 0)), 1));
        // ...then double-clicks the same word
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        // the double-click selects the word, not the whole line
        assert_eq!(app.buffer.selected_text().as_deref(), Some("brave"));
    }

    #[test]
    fn drag_after_double_click_extends_by_word() {
        let dir = scratch("mdblworddrag");
        let file = dir.join("a.txt");
        fs::write(&file, "hello brave new world\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        let click = |kind| mouse(kind, 37, 1); // char 6 = 'b' of "brave"
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        // drag into "world" (char 16) -> extends word-wise
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 47, 1));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        assert_eq!(
            app.buffer.selected_text().as_deref(),
            Some("brave new world")
        );
    }

    #[test]
    fn drag_jitter_inside_word_keeps_word_selection() {
        let dir = scratch("mdbljitter");
        let file = dir.join("a.txt");
        fs::write(&file, "hello brave world\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        let click = |kind| mouse(kind, 37, 1); // char 6 = 'b' of "brave"
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        // hand jitter while holding the second click: still inside "brave"
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 39, 1));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        assert_eq!(app.buffer.selected_text().as_deref(), Some("brave"));
    }

    #[test]
    fn drag_after_triple_click_extends_by_line() {
        let dir = scratch("mtripledrag");
        let file = dir.join("a.txt");
        fs::write(&file, "one two three\nfour five\nsix\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        let click = |kind| mouse(kind, 31, 1); // char 0 of line 0
        for _ in 0..3 {
            app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
            app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        }
        // drag to line 2 (terminal row 3) -> extends line-wise
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 31, 3));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        assert_eq!(
            app.buffer.selected_text().as_deref(),
            Some("one two three\nfour five\nsix")
        );
    }

    #[test]
    fn multi_click_count_resets_on_new_position() {
        let dir = scratch("mmulticlick");
        let file = dir.join("a.txt");
        fs::write(&file, "hello brave world\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        // double-click "brave" -> word selected
        let click = |kind| mouse(kind, 37, 1);
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        assert_eq!(app.buffer.selected_text().as_deref(), Some("brave"));

        // clicking a different position right away is a plain click again
        let other = mouse(MouseEventKind::Down(MouseButton::Left), 31, 1);
        app.handle_mouse(other);
        assert!(!app.buffer.has_selection());
        assert_eq!(app.buffer.cursor, (0, 0));
    }

    #[test]
    fn double_click_on_whitespace_selects_whitespace() {
        let dir = scratch("mdblspace");
        let file = dir.join("a.txt");
        fs::write(&file, "hello world\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        // terminal col 36 = char 5 = the space between the words
        let click = |kind| mouse(kind, 36, 1);
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        assert_eq!(app.buffer.selected_text().as_deref(), Some(" "));
        assert_eq!(app.buffer.selection_range(), Some(((5, 0), (6, 0))));
    }

    #[test]
    fn mouse_wheel_scrolls_editor_and_sidebar() {
        let dir = scratch("mwheel");
        let file = dir.join("long.txt");
        let content: String = (0..30).map(|i| format!("line {i}\n")).collect();
        fs::write(&file, content).unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        assert_eq!(app.buffer.cursor, (0, 0));
        app.handle_mouse(mouse(MouseEventKind::ScrollDown, 60, 10));
        assert_eq!(app.buffer.cursor, (0, 3));
        app.handle_mouse(mouse(MouseEventKind::ScrollUp, 60, 10));
        assert_eq!(app.buffer.cursor, (0, 0));

        // wheel over the sidebar moves the selection
        app.handle_mouse(mouse(MouseEventKind::ScrollDown, 5, 5));
        assert_eq!(app.sidebar.selected, 1);
    }

    #[test]
    fn sidebar_single_click_selects_double_click_opens() {
        let dir = scratch("mdblclick");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = App::new(dir, None).unwrap();
        render_buffer(&mut app);

        // single click on the file row (row 1: ".." is row 0)
        let click = |kind| mouse(kind, 5, 2);
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        assert_eq!(app.sidebar.selected, 1);
        assert_eq!(app.focus, Focus::Sidebar);
        assert!(app.buffer.path.is_none()); // not opened yet

        // second click within the double-click window opens the file
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        assert_eq!(app.focus, Focus::Editor);
        assert_eq!(app.buffer.lines, vec!["alpha"]);
    }

    #[test]
    fn sidebar_double_click_on_directory_descends() {
        let dir = scratch("mdblclickdir");
        fs::create_dir(dir.join("sub")).unwrap();
        let mut app = App::new(dir.clone(), None).unwrap();
        render_buffer(&mut app);

        let click = |kind| mouse(kind, 5, 1); // row 0: ".."
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        // opened the parent of the scratch dir
        assert_ne!(app.sidebar.dir, dir);
    }

    // ---- clipboard ---------------------------------------------------------

    #[test]
    fn copy_cut_paste_roundtrip() {
        let dir = scratch("clip1");
        let mut app = with_fake_clipboard(App::new(dir, None).unwrap());
        app.handle_key(ctrl('o'));
        for c in "hello world".chars() {
            app.handle_key(char_key(c));
        }
        // select "world" with shift+arrows
        app.handle_key(key(KeyCode::Home));
        for _ in 0..6 {
            app.handle_key(key(KeyCode::Right));
        }
        for _ in 0..5 {
            app.handle_key(shift_key(KeyCode::Right));
        }
        app.handle_key(ctrl('x')); // cut
        assert_eq!(app.buffer.lines, vec!["hello "]);
        assert_eq!(app.clipboard.get_text().as_deref(), Some("world"));

        app.handle_key(key(KeyCode::End));
        app.handle_key(ctrl('v')); // paste
        assert_eq!(app.buffer.lines, vec!["hello world"]);
    }

    #[test]
    fn select_all_copy_and_paste_replaces() {
        let dir = scratch("clip2");
        let mut app = with_fake_clipboard(App::new(dir, None).unwrap());
        app.handle_key(ctrl('o'));
        for c in "abc".chars() {
            app.handle_key(char_key(c));
        }
        app.handle_key(key(KeyCode::Enter));
        for c in "def".chars() {
            app.handle_key(char_key(c));
        }
        app.handle_key(ctrl('a'));
        app.handle_key(ctrl('c'));
        assert_eq!(app.clipboard.get_text().as_deref(), Some("abc\ndef"));

        // pasting over the selection replaces it (no duplication)
        app.handle_key(ctrl('v'));
        assert_eq!(app.buffer.lines, vec!["abc", "def"]);
        assert!(!app.buffer.has_selection());
    }

    #[test]
    fn cmd_shortcuts_work_like_ctrl() {
        let dir = scratch("clip3");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha").unwrap();
        let mut app = with_fake_clipboard(App::new(dir.clone(), Some(file)).unwrap());
        app.handle_key(char_key('X'));
        assert!(app.buffer.dirty);
        app.handle_key(cmd('s')); // Cmd+S saves
        assert!(!app.buffer.dirty);
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "Xalpha");

        app.handle_key(cmd('o')); // Cmd+O switches focus
        assert_eq!(app.focus, Focus::Sidebar);
        app.handle_key(cmd('o'));
        assert_eq!(app.focus, Focus::Editor);
        app.handle_key(cmd('q')); // Cmd+Q quits
        assert!(app.should_quit);
    }

    #[test]
    fn bracketed_paste_inserts_multiline() {
        let dir = scratch("clip4");
        let mut app = App::new(dir, None).unwrap();
        app.handle_key(ctrl('o'));
        app.handle_key(char_key('a'));
        app.paste_text("b\nc".to_string());
        assert_eq!(app.buffer.lines, vec!["ab", "c"]);
        assert!(app.buffer.dirty);
    }

    #[test]
    fn paste_with_selection_replaces_it() {
        let dir = scratch("clip5");
        let mut app = with_fake_clipboard(App::new(dir, None).unwrap());
        app.handle_key(ctrl('o'));
        for c in "abcdef".chars() {
            app.handle_key(char_key(c));
        }
        app.handle_key(key(KeyCode::Home));
        for _ in 0..3 {
            app.handle_key(shift_key(KeyCode::Right));
        }
        app.clipboard.set_text("XYZ");
        app.handle_key(ctrl('v'));
        assert_eq!(app.buffer.lines, vec!["XYZdef"]);
    }

    #[test]
    fn partial_selection_splits_highlighted_span() {
        let line = "fn main";
        let syntax_style = Style::default().fg(Color::Blue);
        let spans = clip_ops(
            line,
            &[(Some(syntax_style), 0..line.len())],
            0,
            line.chars().count(),
            Some((3, 5)),
        );

        assert_eq!(
            spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<Vec<_>>(),
            vec!["fn ", "ma", "in"]
        );
        assert!(!spans[0].style.add_modifier.contains(Modifier::REVERSED));
        assert!(spans[1].style.add_modifier.contains(Modifier::REVERSED));
        assert!(!spans[2].style.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(spans[0].style.fg, Some(Color::Blue));
        assert_eq!(spans[1].style.fg, Some(Color::Blue));
        assert_eq!(spans[2].style.fg, Some(Color::Blue));
    }

    #[test]
    fn selection_renders_reversed() {
        let dir = scratch("mselrender");
        let file = dir.join("a.txt");
        fs::write(&file, "hello world\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        app.buffer.home();
        app.buffer.begin_selection();
        app.buffer.end();
        let buf = render_buffer(&mut app);
        // cells inside the selection are reversed
        for x in 31..42 {
            assert!(
                buf.cell((x, 1))
                    .unwrap()
                    .style()
                    .add_modifier
                    .contains(Modifier::REVERSED),
                "col {x}"
            );
        }
        // cells outside are not
        assert!(
            !buf.cell((43, 1))
                .unwrap()
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    // ---- undo / redo ------------------------------------------------------

    fn ctrl_shift(c: char) -> KeyEvent {
        KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        )
    }

    fn cmd_shift(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::SUPER | KeyModifiers::SHIFT)
    }

    #[test]
    fn ctrl_z_undoes_and_ctrl_shift_z_redoes() {
        let dir = scratch("undoredo1");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha").unwrap();
        let mut app = App::new(dir.clone(), Some(file)).unwrap();

        app.handle_key(char_key('X'));
        assert_eq!(app.buffer.lines, vec!["Xalpha"]);
        assert!(app.buffer.dirty);

        app.handle_key(ctrl('z'));
        assert_eq!(app.buffer.lines, vec!["alpha"]);
        assert!(!app.buffer.dirty);
        assert_eq!(app.buffer.cursor, (0, 0));

        app.handle_key(ctrl_shift('z'));
        assert_eq!(app.buffer.lines, vec!["Xalpha"]);
        assert!(app.buffer.dirty);
        assert_eq!(app.buffer.cursor, (1, 0));
    }

    #[test]
    fn cmd_z_and_cmd_shift_z_work_like_ctrl() {
        let dir = scratch("undoredo2");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha").unwrap();
        let mut app = App::new(dir.clone(), Some(file)).unwrap();

        app.handle_key(char_key('X'));
        app.handle_key(cmd('z'));
        assert_eq!(app.buffer.lines, vec!["alpha"]);
        app.handle_key(cmd_shift('z'));
        assert_eq!(app.buffer.lines, vec!["Xalpha"]);
    }

    #[test]
    fn undo_works_from_sidebar_focus() {
        let dir = scratch("undoredo4");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha").unwrap();
        let mut app = App::new(dir.clone(), Some(file)).unwrap();

        app.handle_key(char_key('X'));
        app.handle_key(ctrl('o')); // switch to the sidebar
        assert_eq!(app.focus, Focus::Sidebar);
        app.handle_key(ctrl('z'));
        assert_eq!(app.buffer.lines, vec!["alpha"]);
    }

    #[test]
    fn ctrl_z_in_save_as_prompt_leaves_buffer_alone() {
        let dir = scratch("undoredo5");
        let mut app = App::new(dir, None).unwrap();
        app.handle_key(ctrl('o'));
        app.handle_key(char_key('x'));
        app.handle_key(ctrl('s')); // no file name yet: save-as prompt
        assert!(app.save_as_input.is_some());

        app.handle_key(ctrl('z'));
        assert!(app.save_as_input.is_some());
        assert_eq!(app.buffer.lines, vec!["x"]);
    }

    #[test]
    fn undo_redo_round_trip_across_save() {
        let dir = scratch("undoredo6");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha").unwrap();
        let mut app = App::new(dir.clone(), Some(file)).unwrap();

        app.handle_key(char_key('X'));
        app.handle_key(ctrl('s')); // save: "Xalpha" on disk
        assert!(!app.buffer.dirty);

        // undoing past the save restores the pre-edit text; the buffer
        // now differs from what is on disk, so it is dirty again
        app.handle_key(ctrl('z'));
        assert_eq!(app.buffer.lines, vec!["alpha"]);
        assert!(app.buffer.dirty);
        // redoing brings back the saved content, which matches the disk
        app.handle_key(ctrl_shift('z'));
        assert_eq!(app.buffer.lines, vec!["Xalpha"]);
        assert!(!app.buffer.dirty);
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "Xalpha");
    }

    #[test]
    fn undo_rehighlights_restored_text() {
        let dir = scratch("undoredo7");
        let file = dir.join("code.rs");
        fs::write(&file, "fn main() {\n}\n").unwrap();
        let mut app = App::new(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);
        assert_ne!(buf.cell((31, 1)).unwrap().style().fg, Some(Color::Reset));

        // typing breaks the keyword, undo restores both text and color
        app.handle_key(char_key('x'));
        let buf = render_buffer(&mut app);
        assert_eq!(buf.cell((31, 1)).unwrap().style().fg, Some(Color::Reset));

        app.handle_key(ctrl('z'));
        let buf = render_buffer(&mut app);
        assert_eq!(buf.cell((31, 1)).unwrap().symbol(), "f");
        assert_eq!(
            buf.cell((31, 1)).unwrap().style().fg,
            Some(Color::Rgb(180, 142, 173))
        );
    }

    #[test]
    fn status_bar_lists_undo_shortcut() {
        let dir = scratch("undoredo8");
        let mut app = App::new(dir, None).unwrap();
        let rows = render(&mut app);
        assert!(row_contains(&rows, "Ctrl+Z undo"));
    }
}
