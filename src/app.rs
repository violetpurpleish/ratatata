//! Application state, key handling and rendering.

/// Fold a reported character into what the user actually typed.
///
/// Kitty-protocol terminals that encode Shift+letter as a CSI u event
/// report the *unshifted* base key plus a SHIFT modifier (e.g. `a`+SHIFT
/// for `A`), because the shifted character is layout-dependent. ASCII
/// letters can be folded deterministically; symbols cannot (Shift+8 is
/// `(` on a German layout but `*` on a US one), so they are left alone.
fn printable_char(c: char, modifiers: KeyModifiers) -> char {
    if modifiers.contains(KeyModifiers::SHIFT) {
        c.to_ascii_uppercase()
    } else {
        c
    }
}

use std::io;
use std::ops::Range;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use ratatui_themes::{Theme, ThemeName, ThemePalette};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::buffer::{Buffer, char_width, expand_tabs, visual_chunk, visual_row_of};
use crate::clipboard::{Clipboard, SystemClipboard};
use crate::highlight::Highlighter;
use crate::image_view::{self, ImagePreview};
use crate::search::Search;
use crate::sidebar::{Kind, Sidebar};
use ratatui_image::FontSize;
use ratatui_image::picker::Picker;

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
/// The shortcut bar at the top wraps onto a second row when the buttons
/// don't fit; anything beyond that is omitted (with an ellipsis marker).
const TOPBAR_MAX_ROWS: u16 = 2;
/// Separator between the shortcut buttons in the top bar.
const TOPBAR_SEPARATOR: &str = " | ";
const TOPBAR_SEPARATOR_WIDTH: u16 = 3;
/// Left margin of the top bar: the first button must not start at cell
/// (0,0), because most terminals report a synthetic (0,0) event when the
/// mouse leaves the window.
const TOPBAR_INDENT: u16 = 1;
/// The fixed application theme. The palette is copied out of the theme so
/// rendering helpers can use semantic colors without reconstructing it.
const THEME: Theme = Theme::new(ThemeName::CatppuccinMocha);
const PALETTE: ThemePalette = THEME.palette();
/// Accent used for whichever panel and scrollbar currently have focus.
const FOCUS_COLOR: Color = PALETTE.accent;

/// Background used while a shortcut button is hovered, so it reads as
/// "about to be clicked". Idle buttons stay on the theme's base background.
const TOPBAR_PILL_BG_HOVER: Color = PALETTE.muted;

/// Background of the current search match (same yellow as the block caret).
const SEARCH_CURRENT_BG: Color = PALETTE.warning;
/// Background of the other search matches.
const SEARCH_OTHER_BG: Color = PALETTE.selection;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Sidebar,
    Editor,
}

/// The actions behind the keyboard shortcuts and the clickable buttons in
/// the top bar. Both input paths route through [`App::invoke_shortcut`], so
/// the keys and the mouse always behave identically.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shortcut {
    Quit,
    SwitchFocus,
    Save,
    Copy,
    Cut,
    Paste,
    SelectAll,
    Find,
    ToggleWrap,
    Undo,
    Redo,
    ClosePreview,
    NewFile,
}

impl Shortcut {
    /// The key combo shown on the button.
    fn key_label(self) -> &'static str {
        match self {
            Shortcut::Quit => "Ctrl+Q",
            Shortcut::SwitchFocus => "Ctrl+O",
            Shortcut::Save => "Ctrl+S",
            Shortcut::Copy => "Ctrl+C",
            Shortcut::Cut => "Ctrl+X",
            Shortcut::Paste => "Ctrl+V",
            Shortcut::SelectAll => "Ctrl+A",
            Shortcut::Find => "Ctrl+F",
            Shortcut::ToggleWrap => "Ctrl+W",
            Shortcut::Undo => "Ctrl+Z",
            Shortcut::Redo => "Ctrl+Shift+Z",
            Shortcut::ClosePreview => "Esc",
            Shortcut::NewFile => "Ctrl+N",
        }
    }

    /// The short action name shown on the button.
    fn action_label(self) -> &'static str {
        match self {
            Shortcut::Quit => "quit",
            Shortcut::SwitchFocus => "switch",
            Shortcut::Save => "save",
            Shortcut::Copy => "copy",
            Shortcut::Cut => "cut",
            Shortcut::Paste => "paste",
            Shortcut::SelectAll => "select all",
            Shortcut::Find => "search",
            Shortcut::ToggleWrap => "wrap",
            Shortcut::Undo => "undo",
            Shortcut::Redo => "redo",
            Shortcut::ClosePreview => "close preview",
            Shortcut::NewFile => "new",
        }
    }

    /// Longer description shown in the status bar while the button is
    /// hovered.
    fn description(self) -> &'static str {
        match self {
            Shortcut::Quit => "quit — press again to confirm unsaved changes",
            Shortcut::SwitchFocus => "switch between sidebar and editor",
            Shortcut::Save => "save the current file (asks for a name if untitled)",
            Shortcut::Copy => "copy the selection to the clipboard",
            Shortcut::Cut => "cut the selection to the clipboard",
            Shortcut::Paste => "paste from the clipboard",
            Shortcut::SelectAll => "select the whole buffer",
            Shortcut::Find => "search — type to filter, Enter/Shift+Enter next/prev, Esc closes",
            Shortcut::ToggleWrap => "toggle word wrapping of long lines",
            Shortcut::Undo => "undo the last edit",
            Shortcut::Redo => "redo the last undone edit",
            Shortcut::ClosePreview => "close the image preview",
            Shortcut::NewFile => "start a new untitled buffer",
        }
    }

    /// Accent color of the key combo on the button.
    fn key_color(self) -> Color {
        match self {
            Shortcut::Quit => PALETTE.error,
            Shortcut::SwitchFocus => PALETTE.info,
            Shortcut::Save => PALETTE.success,
            Shortcut::Find => PALETTE.warning,
            Shortcut::ToggleWrap => PALETTE.secondary,
            Shortcut::NewFile => PALETTE.accent,
            _ => PALETTE.fg,
        }
    }
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
    /// Terminal graphics picker (protocol + backing-pixel cell size), used
    /// to build image previews.
    picker: Picker,
    /// Logical cell size used for the preview's no-upscaling decision.
    logical_cell_size: FontSize,
    /// Active image preview replacing the text buffer, `None` while editing
    /// text.
    image: Option<ImagePreview>,
    /// Transient status message with expiry.
    message: Option<(String, Instant)>,
    /// Active "save as" input text, when the buffer has no file name.
    save_as_input: Option<String>,
    /// Active incremental search (Ctrl+F), `None` while not searching.
    search: Option<Search>,
    /// Set when Ctrl+Q is pressed with unsaved changes; second press quits.
    quit_armed: bool,
    /// Viewport sizes from the last draw, used for paging and scrolling.
    editor_text: (u16, u16),
    /// Cursor position from the last editor draw. The editor only reanchors
    /// the viewport when this changes (or when the viewport is resized), so
    /// an independent mouse-wheel scroll is not immediately undone by the
    /// next draw.
    last_drawn_cursor: Option<(usize, usize)>,
    sidebar_height: u16,
    /// Widget areas from the last draw, used for mouse hit-testing.
    sidebar_area: Rect,
    editor_area: Rect,
    /// The shortcut-button bar at the top: the bar's area (for clicks that
    /// land on the bar itself) and each button's rectangle from the last
    /// draw.
    topbar_area: Rect,
    topbar_buttons: Vec<(Shortcut, Rect)>,
    /// Shortcut button the mouse is currently over, whose description is
    /// shown in the status bar.
    hovered: Option<Shortcut>,
    /// Last click on the sidebar, for double-click detection.
    last_sidebar_click: Option<(Instant, usize)>,
    /// Last click in the editor, for double-click (word) and triple-click
    /// (line) selection.
    last_editor_click: Option<EditorClick>,
}

impl App {
    pub fn new_with_cell_size(
        dir: PathBuf,
        file: Option<PathBuf>,
        picker: Picker,
        logical_cell_size: FontSize,
    ) -> io::Result<Self> {
        let mut highlighter = Highlighter::new();
        highlighter.set_path(file.as_deref());
        // An image file starts an image preview instead of a text buffer;
        // decoding failures are errors like read failures for text files.
        let (buffer, image) = match &file {
            Some(path) if image_view::is_image_path(path) => (
                Buffer::empty(),
                Some(ImagePreview::open_with_cell_size(
                    path.clone(),
                    &picker,
                    logical_cell_size,
                )?),
            ),
            Some(path) => (Buffer::from_path(path.clone())?, None),
            None => (Buffer::empty(), None),
        };
        let focus = if file.is_some() {
            Focus::Editor
        } else {
            Focus::Sidebar
        };
        let mut sidebar = Sidebar::new(dir)?;
        if let Some(name) = file.as_ref().and_then(|p| p.file_name()) {
            sidebar.select_name(&name.to_string_lossy());
        }
        Ok(Self {
            buffer,
            sidebar,
            focus,
            should_quit: false,
            highlighter,
            clipboard: Box::new(SystemClipboard::new()),
            picker,
            logical_cell_size,
            image,
            message: None,
            save_as_input: None,
            search: None,
            quit_armed: false,
            editor_text: (0, 0),
            last_drawn_cursor: None,
            sidebar_height: 0,
            sidebar_area: Rect::default(),
            editor_area: Rect::default(),
            topbar_area: Rect::default(),
            topbar_buttons: Vec::new(),
            hovered: None,
            last_sidebar_click: None,
            last_editor_click: None,
        })
    }

    fn set_message(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now() + MESSAGE_TTL));
    }

    /// Re-read terminal metrics after a resize without re-querying graphics
    /// protocol support. The image data remains decoded; only its cached
    /// protocol is invalidated so it is rebuilt on the next draw.
    pub(crate) fn refresh_terminal_metrics(&mut self) {
        let (picker, logical_cell_size) = crate::refresh_image_picker(&self.picker);
        self.update_image_metrics(picker, logical_cell_size);
    }

    fn update_image_metrics(&mut self, picker: Picker, logical_cell_size: FontSize) {
        self.picker = picker.clone();
        self.logical_cell_size = logical_cell_size;
        if let Some(preview) = &mut self.image {
            preview.update_metrics(picker, logical_cell_size);
        }
    }

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
                KeyCode::Char('s') => Shortcut::Save,
                KeyCode::Char('c') => Shortcut::Copy,
                KeyCode::Char('x') => Shortcut::Cut,
                KeyCode::Char('v') => Shortcut::Paste,
                KeyCode::Char('a') => Shortcut::SelectAll,
                KeyCode::Char('f') => Shortcut::Find,
                KeyCode::Char('w') => Shortcut::ToggleWrap,
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
        if let Some(search) = self.search.as_mut() {
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
                KeyCode::Backspace => {
                    search.query.pop();
                    query_changed = true;
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
    fn invoke_shortcut(&mut self, action: Shortcut) {
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
                // not while the save-as prompt is modal
                if self.save_as_input.is_none() {
                    self.focus = match self.focus {
                        Focus::Sidebar => Focus::Editor,
                        Focus::Editor => Focus::Sidebar,
                    };
                }
            }
            Shortcut::ClosePreview => {
                if self.image.is_some() {
                    self.close_image_preview();
                }
            }
            Shortcut::NewFile => {
                // not while the save-as prompt or the search bar is modal
                if self.save_as_input.is_none() && self.search.is_none() {
                    self.new_file();
                }
            }
            // While an image preview is open the remaining shortcuts do
            // nothing: there is no text to edit, save or search. This arm
            // comes after Quit/SwitchFocus/ClosePreview/NewFile (which
            // still work) and before the rest.
            _ if self.image.is_some() => {}
            Shortcut::Save => self.save(),
            Shortcut::Copy => {
                if self.save_as_input.is_none() && self.search.is_none() {
                    self.copy_selection();
                }
            }
            Shortcut::Cut => {
                if self.save_as_input.is_none() && self.search.is_none() {
                    self.cut_selection();
                }
            }
            Shortcut::Paste => self.paste_clipboard(),
            Shortcut::SelectAll => {
                if self.save_as_input.is_none() && self.search.is_none() {
                    self.buffer.select_all();
                }
            }
            Shortcut::Find => self.open_search(),
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
                if self.save_as_input.is_none() && self.search.is_none() {
                    self.undo();
                }
            }
            Shortcut::Redo => {
                if self.save_as_input.is_none() && self.search.is_none() {
                    self.redo();
                }
            }
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
            KeyCode::Enter | KeyCode::Right => self.open_selected(),
            KeyCode::Backspace | KeyCode::Left => match self.sidebar.ascend() {
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
        if let Some(search) = self.search.as_mut() {
            search
                .query
                .extend(text.chars().filter(|c| !c.is_control()));
            self.recompute_search();
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
        // bracketed paste while searching fills in the query instead
        if let Some(search) = self.search.as_mut() {
            search
                .query
                .extend(text.chars().filter(|c| !c.is_control()));
            self.recompute_search();
            return;
        }
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

    fn ensure_cursor_visible(&mut self) {
        let (w, h) = self.editor_text;
        self.buffer.ensure_visible(h as usize, w as usize);
    }

    /// Move the editor viewport without moving or reanchoring the cursor.
    fn scroll_editor(&mut self, rows: isize) {
        self.buffer
            .scroll_vertical(rows, self.editor_text.1 as usize);
    }

    fn in_topbar(&self, pos: (usize, usize)) -> bool {
        self.topbar_area
            .contains(Position::new(pos.0 as u16, pos.1 as u16))
    }

    /// The shortcut button under a mouse position, if any. Cell (0,0) is
    /// deliberately never a hit: the top bar starts one cell in, and
    /// terminals that report a synthetic (0,0) event when the mouse
    /// leaves the window must not light up (or trigger) anything.
    fn topbar_action_at(&self, pos: (usize, usize)) -> Option<Shortcut> {
        if pos == (0, 0) {
            return None;
        }
        let p = Position::new(pos.0 as u16, pos.1 as u16);
        self.topbar_buttons
            .iter()
            .find(|(_, rect)| rect.contains(p))
            .map(|(action, _)| *action)
    }

    fn in_sidebar(&self, pos: (usize, usize)) -> bool {
        self.sidebar_area
            .contains(Position::new(pos.0 as u16, pos.1 as u16))
    }

    fn in_editor(&self, pos: (usize, usize)) -> bool {
        self.editor_area
            .contains(Position::new(pos.0 as u16, pos.1 as u16))
    }

    /// Whether a position is over rendered editor text rather than its border
    /// or line-number gutter. This prevents a Ctrl/Cmd-click on the gutter
    /// from opening a URL that happens to start at column zero.
    fn in_editor_text(&self, pos: (usize, usize)) -> bool {
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
    fn open_link_at(&mut self, click: Option<(usize, usize)>) -> bool {
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
    fn is_ctrl_or_cmd(modifiers: KeyModifiers) -> bool {
        modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
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

    // ---- file operations ---------------------------------------------------

    /// Drop the current buffer (and any image preview) and start a fresh
    /// untitled one — the same state as launching the app from a directory
    /// without a file path, except that focus stays on the editor so the
    /// new file can be typed into immediately. Refuses while the buffer
    /// has unsaved changes.
    fn new_file(&mut self) {
        if self.buffer.dirty {
            self.set_message("unsaved changes — press Ctrl+S to save first");
            return;
        }
        self.buffer = Buffer::empty();
        self.image = None;
        self.highlighter.set_path(None);
        self.quit_armed = false;
        self.focus = Focus::Editor;
        self.set_message("new file — press Ctrl+S to save");
    }

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

    // ---- search ------------------------------------------------------------

    /// Open the search bar. Ctrl+F while it is already open jumps to the
    /// next match (like most editors).
    fn open_search(&mut self) {
        if self.save_as_input.is_some() {
            return;
        }
        if self.search.is_some() {
            self.search_step(1);
        } else {
            self.search = Some(Search::new());
        }
    }

    /// Move the current match by `dir` (+1 next, −1 previous) and jump the
    /// cursor to it.
    fn search_step(&mut self, dir: isize) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        search.step(dir);
        self.jump_to_current_match();
    }

    /// Recompute the matches after the query changed and jump the cursor to
    /// the match at or after its current position.
    fn recompute_search(&mut self) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let cursor = self.buffer.cursor;
        search.refresh(&self.buffer.lines, cursor);
        self.jump_to_current_match();
    }

    /// Move the cursor onto the current search match and clear any
    /// selection so the highlight reads cleanly.
    fn jump_to_current_match(&mut self) {
        let Some(search) = self.search.as_ref() else {
            return;
        };
        let Some(m) = search.current_match() else {
            return;
        };
        self.buffer.cursor = (m.start, m.line);
        self.buffer.clear_selection();
        self.ensure_cursor_visible();
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
        if image_view::is_image_path(&path) {
            match ImagePreview::open_with_cell_size(
                path.clone(),
                &self.picker,
                self.logical_cell_size,
            ) {
                Ok(preview) => {
                    self.buffer = Buffer::empty();
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
                self.buffer = buffer;
                self.highlighter.set_path(Some(&path));
                self.quit_armed = false;
                self.focus = Focus::Editor;
                self.set_message(format!("opened {}", path.display()));
            }
            Err(e) => self.set_message(format!("cannot open {}: {e}", path.display())),
        }
    }

    /// Close the image preview and drop back to an empty buffer. The
    /// terminal's copy of the transmitted image is freed when the app exits.
    fn close_image_preview(&mut self) {
        self.image = None;
        self.buffer = Buffer::empty();
        self.focus = Focus::Editor;
    }

    // ---- drawing -----------------------------------------------------------

    pub fn draw(&mut self, frame: &mut Frame) {
        // Paint the terminal with the theme's base color first. Individual
        // widgets and syntax spans then layer their semantic foregrounds and
        // backgrounds over it.
        frame.render_widget(
            Block::default().style(Style::default().bg(PALETTE.bg)),
            frame.area(),
        );

        let pills = self.shortcut_pills();
        let [top_area, main, status_area] = Layout::vertical([
            Constraint::Length(shortcut_bar_height(&pills, frame.area().width)),
            Constraint::Min(0),
            Constraint::Length(STATUS_HEIGHT),
        ])
        .areas(frame.area());
        let [side_area, edit_area] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(main);
        self.topbar_area = top_area;
        self.sidebar_area = side_area;
        self.editor_area = edit_area;

        self.draw_topbar(frame, top_area, &pills);
        self.draw_sidebar(frame, side_area);
        self.draw_editor(frame, edit_area);
        self.draw_status(frame, status_area);
    }

    /// The shortcut buttons shown in the top bar for the current mode:
    /// the full editing set, or the short preview-only set while an image
    /// is open.
    fn shortcut_pills(&self) -> Vec<Shortcut> {
        if self.image.is_some() {
            vec![
                Shortcut::ClosePreview,
                Shortcut::SwitchFocus,
                Shortcut::Quit,
            ]
        } else {
            vec![
                Shortcut::SwitchFocus,
                Shortcut::NewFile,
                Shortcut::Save,
                Shortcut::Undo,
                Shortcut::Redo,
                Shortcut::Copy,
                Shortcut::Cut,
                Shortcut::Paste,
                Shortcut::Find,
                Shortcut::ToggleWrap,
                Shortcut::Quit,
            ]
        }
    }

    /// The clickable shortcut buttons ("Ctrl+O switch", …) across the top,
    /// separated by " | " and indented one cell from the window edge.
    /// Buttons flow onto a second row when they don't fit; anything still
    /// left over is omitted with an ellipsis marker. Button rectangles
    /// from the last draw are kept for mouse hit-testing.
    fn draw_topbar(&mut self, frame: &mut Frame, area: Rect, pills: &[Shortcut]) {
        self.topbar_buttons.clear();
        let hovered = self.hovered;
        let mut lines: Vec<Line> = Vec::new();
        // every row begins with a one-cell margin so the first button
        // never sits at the window edge at (0,0)
        let mut row_spans: Vec<Span<'static>> = vec![Span::raw(" ")];
        let mut x = area.x + TOPBAR_INDENT;
        for &action in pills {
            let w = pill_width(action);
            if x + w > area.x + area.width {
                // doesn't fit on this row: start the next one
                lines.push(Line::from(std::mem::take(&mut row_spans)));
                x = area.x + TOPBAR_INDENT;
                row_spans.push(Span::raw(" "));
                if lines.len() as u16 >= TOPBAR_MAX_ROWS {
                    // out of rows: mark the overflow and stop
                    if let Some(last) = lines.last_mut() {
                        last.spans
                            .push(Span::styled("…", Style::default().fg(PALETTE.muted)));
                    }
                    break;
                }
                if x + w > area.x + area.width {
                    // even an empty row cannot hold this button: skip it
                    continue;
                }
            }
            // a separator between the buttons (never before the first one
            // on a row, so rows don't end with a dangling pipe)
            if row_spans.len() > 1 {
                row_spans.push(Span::styled(
                    TOPBAR_SEPARATOR,
                    Style::default().fg(PALETTE.muted),
                ));
            }
            self.topbar_buttons
                .push((action, Rect::new(x, area.y + lines.len() as u16, w, 1)));
            // the hovered button is drawn highlighted
            row_spans.extend(pill_spans(action, hovered == Some(action)));
            x += w + TOPBAR_SEPARATOR_WIDTH;
        }
        if !row_spans.is_empty() || lines.is_empty() {
            lines.push(Line::from(row_spans));
        }
        for (i, line) in lines.iter().enumerate() {
            frame.render_widget(
                Paragraph::new(line.clone()),
                Rect::new(area.x, area.y + i as u16, area.width, 1),
            );
        }
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
                Kind::Parent => Style::default().fg(PALETTE.muted),
                Kind::Dir => Style::default().fg(PALETTE.info),
                Kind::File => Style::default().fg(PALETTE.fg),
            };
            if entry.is_hidden() {
                style = style.add_modifier(Modifier::DIM);
            }
            let selected = i == self.sidebar.selected;
            if selected {
                style = style.bg(PALETTE.selection);
            }
            let marker = if selected { "▶ " } else { "  " };
            rows.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(PALETTE.warning)),
                Span::styled(display, style),
            ]));
        }
        if rows.is_empty() {
            rows.push(Line::from(Span::styled(
                "(empty)",
                Style::default().fg(PALETTE.muted),
            )));
        }

        let title = truncate(&self.sidebar.dir.display().to_string(), area.width as usize);
        let block = Block::bordered()
            .title(Span::styled(title, Style::default().fg(PALETTE.info)))
            .title_style(Style::default().add_modifier(Modifier::BOLD))
            .border_style(if self.focus == Focus::Sidebar {
                Style::default().fg(FOCUS_COLOR)
            } else {
                Style::default().fg(PALETTE.muted)
            });
        let paragraph = Paragraph::new(rows)
            .block(block)
            .scroll((self.sidebar.scroll as u16, 0));
        frame.render_widget(paragraph, area);
    }

    fn draw_editor(&mut self, frame: &mut Frame, area: Rect) {
        let title = self
            .image
            .as_ref()
            .map(|preview| preview.path.display().to_string())
            .or_else(|| self.buffer.path.as_ref().map(|p| p.display().to_string()))
            .unwrap_or_else(|| "untitled".to_string());
        let title = truncate(&title, area.width as usize);

        let block = Block::bordered()
            .title(Span::styled(title, Style::default().fg(PALETTE.success)))
            .title_style(Style::default().add_modifier(Modifier::BOLD))
            .border_style(if self.focus == Focus::Editor {
                Style::default().fg(FOCUS_COLOR)
            } else {
                Style::default().fg(PALETTE.muted)
            });

        // An image preview replaces the text: render the block and the image
        // fitted into the inner area (the image keeps its aspect ratio and
        // is never upscaled).
        if let Some(preview) = &mut self.image {
            let inner = block.inner(area);
            frame.render_widget(block, area);
            preview.draw(frame, inner);
            return;
        }

        let inner = block.inner(area);

        let gutter_w = self.buffer.lines.len().to_string().len() + 1;
        let text_w = inner.width.saturating_sub(gutter_w as u16) as usize;
        let text_h = inner.height as usize;
        let viewport = (text_w as u16, text_h as u16);
        let viewport_changed = self.editor_text != viewport;
        self.editor_text = viewport;
        self.buffer.wrap_width = text_w;
        // Clamp a viewport that may have become too far down after an edit,
        // but do not otherwise reanchor it to the cursor. This distinction is
        // what lets the mouse wheel scroll through the file independently.
        self.buffer.clamp_scroll(text_h);
        if viewport_changed || self.last_drawn_cursor != Some(self.buffer.cursor) {
            self.buffer.ensure_visible(text_h, text_w);
        }
        self.last_drawn_cursor = Some(self.buffer.cursor);

        // With wrapping every logical line may occupy several visual
        // rows; `scroll.y` is then a visual row, the gutter shows the
        // line number only on the first row of each line, and the caret
        // sits on the visual row holding the cursor.
        if self.buffer.wrap {
            let width = self.buffer.wrap_width.max(1);
            let mut rows: Vec<Line> = Vec::with_capacity(text_h);
            let mut caret_style = None;
            // the logical line and chunk the viewport starts on
            let (mut y, mut chunk_k) = self
                .buffer
                .vrow_position(self.buffer.scroll.1)
                .unwrap_or((self.buffer.lines.len(), 0));
            let mut remaining = text_h;
            while remaining > 0 && y < self.buffer.lines.len() {
                let line = &self.buffer.lines[y];
                let ops = self.highlighter.highlight_line(&self.buffer.lines, y);
                // selection overlap on this line, in byte offsets
                let sel = self
                    .buffer
                    .selection_on_line(y)
                    .map(|(a, b)| (char_index_to_byte(line, a), char_index_to_byte(line, b)));
                // search matches on this line, in char offsets
                let search_matches: Vec<(usize, usize, bool)> = self
                    .search
                    .as_ref()
                    .map(|s| s.matches_on_line(y).collect())
                    .unwrap_or_default();
                // the chunk the cursor sits on within this line
                let cursor_chunk = if self.focus == Focus::Editor
                    && self.save_as_input.is_none()
                    && self.buffer.cursor.1 == y
                {
                    Some(visual_row_of(line, self.buffer.cursor.0, width))
                } else {
                    None
                };
                loop {
                    if remaining == 0 {
                        break;
                    }
                    let (cstart, cend) = visual_chunk(line, chunk_k, width);
                    let num = if chunk_k == 0 {
                        Span::styled(
                            format!("{:>width$} ", y + 1, width = gutter_w - 1),
                            Style::default().fg(PALETTE.muted),
                        )
                    } else {
                        Span::raw(" ".repeat(gutter_w))
                    };
                    if cursor_chunk == Some(chunk_k) {
                        let cursor_byte = char_index_to_byte(line, self.buffer.cursor.0);
                        caret_style = Some(
                            ops.iter()
                                .find(|(_, range)| range.contains(&cursor_byte))
                                .and_then(|(style, _)| *style)
                                .unwrap_or_default(),
                        );
                    }
                    let mut spans = vec![num];
                    spans.extend(clip_ops(
                        line,
                        ops,
                        cstart,
                        cend - cstart,
                        sel,
                        &search_matches,
                    ));
                    rows.push(Line::from(spans));
                    remaining -= 1;
                    if cend >= line.chars().count() {
                        y += 1;
                        chunk_k = 0;
                        break;
                    }
                    chunk_k += 1;
                }
            }
            if rows.is_empty() {
                rows.push(Line::from(Span::styled(
                    "(empty)",
                    Style::default().fg(PALETTE.muted),
                )));
            }

            let paragraph = Paragraph::new(rows).block(block);
            frame.render_widget(paragraph, area);

            if self.focus == Focus::Editor && self.save_as_input.is_none() {
                let caret_vrow = self.buffer.cursor_vrow();
                if caret_vrow >= self.buffer.scroll.1 && caret_vrow < self.buffer.scroll.1 + text_h
                {
                    let cx = area.x + 1 + gutter_w as u16 + self.buffer.cursor_col() as u16;
                    let cy = area.y + 1 + (caret_vrow - self.buffer.scroll.1) as u16;
                    let inner_right = area.x + area.width.saturating_sub(1);
                    let inner_bottom = area.y + area.height.saturating_sub(1);
                    if cx < inner_right && cy < inner_bottom {
                        let symbol = self
                            .buffer
                            .lines
                            .get(self.buffer.cursor.1)
                            .and_then(|line| line.chars().nth(self.buffer.cursor.0))
                            .map_or_else(
                                || " ".to_string(),
                                |c| {
                                    if c == '\t' {
                                        " ".to_string()
                                    } else {
                                        c.to_string()
                                    }
                                },
                            );
                        let caret = Paragraph::new(Span::styled(
                            symbol,
                            caret_style.unwrap_or_default().bg(PALETTE.warning),
                        ));
                        frame.render_widget(caret, Rect::new(cx, cy, 1, 1));
                    }
                }
            }
            self.draw_editor_scrollbar(frame, area, text_h);
            return;
        }

        let start = self.buffer.scroll.1;
        let end = (start + text_h).min(self.buffer.lines.len());
        let mut rows: Vec<Line> = Vec::with_capacity(end.saturating_sub(start));
        let mut caret_style = None;
        for y in start..end {
            let num = Span::styled(
                format!("{:>width$} ", y + 1, width = gutter_w - 1),
                Style::default().fg(PALETTE.muted),
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
            // search matches on this line, in char offsets
            let search_matches: Vec<(usize, usize, bool)> = self
                .search
                .as_ref()
                .map(|s| s.matches_on_line(y).collect())
                .unwrap_or_default();
            let mut spans = vec![num];
            spans.extend(clip_ops(
                line,
                ops,
                self.buffer.scroll.0,
                text_w,
                sel,
                &search_matches,
            ));
            rows.push(Line::from(spans));
        }
        if rows.is_empty() {
            rows.push(Line::from(Span::styled(
                "(empty)",
                Style::default().fg(PALETTE.muted),
            )));
        }

        let paragraph = Paragraph::new(rows).block(block);
        frame.render_widget(paragraph, area);

        if self.focus == Focus::Editor && self.save_as_input.is_none() {
            // Ratatui can position the terminal cursor, but cannot give it a
            // color. Render a block caret ourselves so it remains distinct
            // from the themed selection style (and leave the native cursor
            // hidden). Independent scrolling can put the caret outside the
            // viewport, in which case there is deliberately nothing to draw.
            let cursor_visible = self.buffer.cursor.1 >= self.buffer.scroll.1
                && self.buffer.cursor.1 < self.buffer.scroll.1.saturating_add(text_h)
                && self.buffer.cursor.0 >= self.buffer.scroll.0
                && self.buffer.cursor.0 <= self.buffer.scroll.0.saturating_add(text_w);
            let cx = area.x + 1 + gutter_w as u16 + self.buffer.cursor_col() as u16;
            let inner_right = area.x + area.width.saturating_sub(1);
            let inner_bottom = area.y + area.height.saturating_sub(1);
            if cursor_visible && cx < inner_right {
                let cy = area.y + 1 + (self.buffer.cursor.1 - self.buffer.scroll.1) as u16;
                if cy < inner_bottom {
                    let symbol = self
                        .buffer
                        .lines
                        .get(self.buffer.cursor.1)
                        .and_then(|line| line.chars().nth(self.buffer.cursor.0))
                        .map_or_else(
                            || " ".to_string(),
                            |c| {
                                if c == '\t' {
                                    " ".to_string()
                                } else {
                                    c.to_string()
                                }
                            },
                        );
                    let caret = Paragraph::new(Span::styled(
                        symbol,
                        caret_style.unwrap_or_default().bg(PALETTE.warning),
                    ));
                    frame.render_widget(caret, Rect::new(cx, cy, 1, 1));
                }
            }
        }
        self.draw_editor_scrollbar(frame, area, text_h);
    }

    /// Draw the vertical editor scrollbar over the block's right border. The
    /// border column is used rather than the text area, so adding the
    /// scrollbar does not steal a column from the editor or change wrapping.
    fn draw_editor_scrollbar(&self, frame: &mut Frame, area: Rect, viewport: usize) {
        let content_length = self.buffer.total_visual_rows();
        let viewport = viewport.max(1);
        if content_length <= viewport || area.width == 0 || area.height <= 2 {
            return;
        }

        let scrollbar_area = Rect::new(area.x + area.width - 1, area.y + 1, 1, area.height - 2);
        // Ratatui's scrollbar position spans the number of possible
        // positions, not the number of content rows. Supplying the number
        // of viewport starts makes `scroll == content - viewport` land on
        // the final scrollbar cell instead of leaving a gap below the thumb.
        let scroll_positions = content_length.saturating_sub(viewport).saturating_add(1);
        let mut state = ScrollbarState::new(scroll_positions)
            .position(self.buffer.scroll.1)
            .viewport_content_length(viewport);
        let thumb_style = if self.focus == Focus::Editor {
            Style::default().fg(FOCUS_COLOR)
        } else {
            Style::default().fg(PALETTE.muted)
        };
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .track_style(Style::default().fg(PALETTE.muted))
            .thumb_style(thumb_style);
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut state);
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
                        .fg(PALETTE.warning)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(input.clone(), Style::default().fg(PALETTE.fg)),
            ]))
            .style(Style::default().bg(PALETTE.bg));
            frame.render_widget(paragraph, area);
            frame.set_cursor_position(Position::new(area.x + prompt_w + input_w, area.y));
            return;
        }

        // the search prompt replaces the status bar content while active
        if let Some(search) = &self.search {
            let prompt = "search: ";
            let prompt_w = prompt.width() as u16;
            let input_w = search
                .query
                .chars()
                .map(|c| c.width().unwrap_or(0))
                .sum::<usize>() as u16;
            let base = Style::default().bg(PALETTE.bg);
            let counter: Vec<Span> = if search.query.is_empty() {
                Vec::new()
            } else if search.match_count() == 0 {
                vec![Span::styled(
                    "no matches",
                    Style::default()
                        .fg(PALETTE.warning)
                        .add_modifier(Modifier::BOLD),
                )]
            } else {
                vec![Span::styled(
                    format!("{}/{}", search.current_index() + 1, search.match_count()),
                    Style::default().fg(PALETTE.info),
                )]
            };
            let counter_w = counter.iter().map(|s| s.content.width() as u16).sum();
            let [left_area, right_area] =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(counter_w)]).areas(area);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        prompt,
                        Style::default()
                            .fg(PALETTE.warning)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(search.query.clone(), Style::default().fg(PALETTE.fg)),
                ]))
                .style(base),
                left_area,
            );
            frame.render_widget(Paragraph::new(Line::from(counter)).style(base), right_area);
            let cursor_x = (area.x + prompt_w + input_w).min(left_area.x + left_area.width);
            frame.set_cursor_position(Position::new(cursor_x, area.y));
            return;
        }

        // right: cursor position, or a description of the shortcut button
        // currently hovered in the top bar
        let (x, y) = self.buffer.cursor;
        let right = match self.hovered {
            Some(action) => action.description().to_string(),
            None if self.image.is_some() => String::new(),
            None => format!("{}:{}", y + 1, x + 1),
        };
        // cap the help so the left side (focus, file, modified state) always
        // stays visible, even on narrow terminals
        let right_width = (right.width() as u16).min(area.width.saturating_sub(24));

        let [left_area, right_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(area);
        let base = Style::default().bg(PALETTE.bg).fg(PALETTE.fg);

        // left: focus + file + modified state, or a transient message
        let (left_spans, left_style): (Vec<Span>, Style) =
            if let Some((msg, expiry)) = &self.message {
                if *expiry > Instant::now() {
                    // keep the modified indicator visible even while a message
                    // is showing
                    let mut spans = vec![Span::styled(
                        msg.clone(),
                        Style::default().fg(PALETTE.warning),
                    )];
                    if self.buffer.dirty {
                        spans.push(Span::styled(
                            " ● modified",
                            Style::default()
                                .fg(PALETTE.warning)
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
        if let Some(preview) = &self.image {
            let tag = "IMAGE";
            let path = preview.path.display().to_string();
            let dims = format!("[{}x{}]", preview.pixels.0, preview.pixels.1);
            let view = "○ view";
            let path_max = width
                .saturating_sub(tag.width() as u16 + dims.width() as u16 + view.width() as u16 + 2);
            let path = truncate(&path, path_max as usize);
            return (
                vec![
                    Span::styled(
                        tag,
                        Style::default()
                            .fg(PALETTE.secondary)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!(" {path} ")),
                    Span::styled(dims, Style::default().fg(PALETTE.muted)),
                    Span::styled(view, Style::default().fg(PALETTE.info)),
                ],
                Style::default(),
            );
        }
        let (tag, tag_color) = match self.focus {
            Focus::Sidebar => ("SIDEBAR", PALETTE.info),
            Focus::Editor => ("EDITOR", PALETTE.success),
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
            format!("[{}] ", self.highlighter.syntax_name())
        } else {
            String::new()
        };
        let wrap = if self.buffer.wrap { "wrap " } else { "" };
        let path_max = width.saturating_sub(
            tag.width() as u16
                + dirty.width() as u16
                + syntax.width() as u16
                + wrap.width() as u16
                + 2,
        );
        let path = truncate(&path, path_max as usize);
        (
            vec![
                Span::styled(
                    tag,
                    Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" {path} ")),
                Span::styled(syntax, Style::default().fg(PALETTE.muted)),
                Span::styled(wrap, Style::default().fg(PALETTE.info)),
                Span::styled(
                    dirty,
                    if self.buffer.dirty {
                        Style::default()
                            .fg(PALETTE.warning)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(PALETTE.success)
                    },
                ),
            ],
            Style::default(),
        )
    }
}

/// Find a web link containing `char_index` in one editor line.
///
/// Link detection deliberately works on the text rather than syntax
/// highlighting, so it also works in plain-text and source files. Markdown
/// punctuation around a URL is ignored, while balanced parentheses inside a
/// URL are retained. `www.` links are normalized to HTTPS before launching.
fn link_at(line: &str, char_index: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    for start in 0..chars.len() {
        let is_http = starts_with_ascii(&chars, start, "http://")
            || starts_with_ascii(&chars, start, "https://");
        let is_www = starts_with_ascii(&chars, start, "www.");
        if !is_http && !is_www {
            continue;
        }
        // Do not treat the middle of an identifier as the start of a link.
        if start > 0 && (chars[start - 1].is_ascii_alphanumeric() || chars[start - 1] == '_') {
            continue;
        }

        let mut end = chars[start..]
            .iter()
            .position(|c| c.is_whitespace())
            .map_or(chars.len(), |offset| start + offset);
        end = trim_link_end(&chars, start, end);
        if start >= end || char_index < start || char_index >= end {
            continue;
        }

        let mut url: String = chars[start..end].iter().collect();
        if is_www {
            url.insert_str(0, "https://");
        }
        return Some(url);
    }
    None
}

/// ASCII case-insensitive prefix matching for URL schemes, whose spelling is
/// case-insensitive even though the rest of a URL is not necessarily so.
fn starts_with_ascii(chars: &[char], start: usize, prefix: &str) -> bool {
    chars
        .get(start..start.saturating_add(prefix.chars().count()))
        .is_some_and(|candidate| {
            candidate
                .iter()
                .zip(prefix.chars())
                .all(|(a, b)| a.eq_ignore_ascii_case(&b))
        })
}

/// Remove punctuation commonly placed after a URL in prose or Markdown.
fn trim_link_end(chars: &[char], start: usize, mut end: usize) -> usize {
    while end > start
        && matches!(
            chars[end - 1],
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"'
        )
    {
        end -= 1;
    }
    while let Some(&closing) = chars.get(end.saturating_sub(1)) {
        let opening = match closing {
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => break,
        };
        let opens = chars[start..end].iter().filter(|&&c| c == opening).count();
        let closes = chars[start..end].iter().filter(|&&c| c == closing).count();
        if closes > opens {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

/// Launch a URL using the operating system's default browser.
fn open_in_browser(url: &str) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn().map(|_| ())
    }
    #[cfg(target_os = "windows")]
    {
        // `start` is a shell built-in; the empty title keeps a URL beginning
        // with a quote from being interpreted as the window title.
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(url).spawn().map(|_| ())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    {
        let _ = url;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no browser launcher for this platform",
        ))
    }
}

/// Rendered width of one shortcut button ("Ctrl+O switch").
fn pill_width(action: Shortcut) -> u16 {
    (action.key_label().width() + action.action_label().width() + 1) as u16
}

/// The spans of one shortcut button: the key combo in bold accent color
/// followed by the action name. Only the hovered button gets a background
/// and brighter label; idle buttons blend into the top bar.
fn pill_spans(action: Shortcut, hovered: bool) -> Vec<Span<'static>> {
    let mut key_style = Style::default()
        .fg(action.key_color())
        .add_modifier(Modifier::BOLD);
    let mut label_style = Style::default().fg(if hovered { PALETTE.fg } else { PALETTE.muted });
    if hovered {
        key_style = key_style.bg(TOPBAR_PILL_BG_HOVER);
        label_style = label_style.bg(TOPBAR_PILL_BG_HOVER);
    }
    vec![
        Span::styled(format!("{} ", action.key_label()), key_style),
        Span::styled(action.action_label(), label_style),
    ]
}

/// How many rows the top bar needs to show all buttons at `width`: one
/// when everything fits (including the left margin and the separators),
/// otherwise two (a second row that is still too small is truncated with
/// an ellipsis marker). Must stay in sync with [`App::draw_topbar`]'s
/// wrapping.
fn shortcut_bar_height(pills: &[Shortcut], width: u16) -> u16 {
    if TOPBAR_INDENT + topbar_total_width(pills) <= width {
        1
    } else {
        TOPBAR_MAX_ROWS
    }
}

/// Combined width of all buttons, including the separators between them.
fn topbar_total_width(pills: &[Shortcut]) -> u16 {
    let n = pills.len() as u16;
    pills.iter().map(|&a| pill_width(a)).sum::<u16>() + TOPBAR_SEPARATOR_WIDTH * n.saturating_sub(1)
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
        width += char_width(c);
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
/// `None` styles render as plain text; spans overlapping `sel` (a byte range
/// on this line) get the theme's selection background, and spans inside a
/// search match (`matches`, char ranges with a "current match" flag) get the
/// match background.
fn clip_ops<'a>(
    line: &'a str,
    ops: &[(Option<Style>, Range<usize>)],
    start_char: usize,
    width: usize,
    sel: Option<(usize, usize)>,
    matches: &[(usize, usize, bool)],
) -> Vec<Span<'a>> {
    let start_byte = char_index_to_byte(line, start_char);
    let end_byte = char_index_to_byte(line, start_char + width);
    let matches: Vec<(usize, usize, bool)> = matches
        .iter()
        .map(|&(a, b, current)| {
            (
                char_index_to_byte(line, a),
                char_index_to_byte(line, b),
                current,
            )
        })
        .collect();
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
        // split the range at every selection and match boundary so each
        // piece can carry its own style
        let mut cuts = vec![a, b];
        if let Some((sa, sb)) = sel {
            cuts.extend([sa, sb]);
        }
        for &(ma, mb, _) in &matches {
            cuts.extend([ma, mb]);
        }
        cuts.sort_unstable();
        cuts.dedup();
        for pair in cuts.windows(2) {
            let (ca, cb) = (pair[0], pair[1]);
            if ca < a || cb > b || ca >= cb {
                continue;
            }
            let mut style = style.unwrap_or_default();
            if let Some(&(_, _, current)) =
                matches.iter().find(|&&(ma, mb, _)| ca >= ma && cb <= mb)
            {
                let bg = if current {
                    SEARCH_CURRENT_BG
                } else {
                    SEARCH_OTHER_BG
                };
                style = style.bg(bg);
            }
            if sel.is_some_and(|(sa, sb)| ca >= sa && cb <= sb) {
                if style.fg.is_none() {
                    style = style.fg(PALETTE.fg);
                }
                style = style.bg(PALETTE.selection);
            }
            let text = expand_tabs(&line[ca..cb]);
            if style == Style::default() {
                out.push(Span::raw(text));
            } else {
                out.push(Span::styled(text, style));
            }
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

    /// App with the deterministic half-blocks picker (no terminal query).
    fn new_app(dir: PathBuf, file: Option<PathBuf>) -> std::io::Result<App> {
        let picker = Picker::halfblocks();
        App::new_with_cell_size(dir, file, picker, image_view::fallback_logical_cell_size())
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

    fn alt(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
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

    #[test]
    fn link_detection_handles_urls_and_surrounding_punctuation() {
        let line = "See https://example.com/docs, and www.example.org/path.";
        let https_col = line.find("https://").unwrap();
        let www_col = line.find("www.").unwrap();
        assert_eq!(
            link_at(line, https_col + 10).as_deref(),
            Some("https://example.com/docs")
        );
        assert_eq!(
            link_at(line, www_col + 5).as_deref(),
            Some("https://www.example.org/path")
        );
        assert_eq!(link_at(line, 0), None);
    }

    #[test]
    fn link_detection_keeps_balanced_url_parentheses() {
        let line = "[site](https://example.com/a_(b)).";
        let col = line.find("example").unwrap();
        assert_eq!(
            link_at(line, col).as_deref(),
            Some("https://example.com/a_(b)")
        );
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
        let app = new_app(dir, Some(file)).unwrap();
        assert_eq!(app.focus, Focus::Editor);
        assert_eq!(app.buffer.lines, vec!["hello", "world", ""]);
        assert!(!app.buffer.dirty);
    }

    #[test]
    fn directory_arg_focuses_sidebar() {
        let dir = scratch("dira");
        let app = new_app(dir, None).unwrap();
        assert_eq!(app.focus, Focus::Sidebar);
        assert!(app.buffer.path.is_none());
    }

    #[test]
    fn ctrl_o_toggles_focus() {
        let dir = scratch("toggle");
        let mut app = new_app(dir, None).unwrap();
        assert_eq!(app.focus, Focus::Sidebar);
        app.handle_key(ctrl('o'));
        assert_eq!(app.focus, Focus::Editor);
        app.handle_key(ctrl('o'));
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[test]
    fn save_as_flow_creates_file_and_updates_sidebar() {
        let dir = scratch("saveas");
        let mut app = new_app(dir.clone(), None).unwrap();

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
        let mut app = new_app(dir, None).unwrap();
        app.handle_key(ctrl('o'));
        app.handle_key(ctrl('s'));
        app.handle_key(char_key('x'));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.save_as_input.is_none());
        assert!(app.buffer.path.is_none());
    }

    // ---- new file (Ctrl+N) ------------------------------------------------

    #[test]
    fn ctrl_n_opens_a_new_untitled_buffer() {
        let dir = scratch("newn");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        assert_eq!(app.buffer.lines, vec!["alpha"]);
        assert!(app.buffer.path.is_some());

        app.handle_key(ctrl('n'));
        assert_eq!(app.buffer.lines, vec![""]);
        assert!(app.buffer.path.is_none());
        assert!(!app.buffer.dirty);
        assert!(app.image.is_none());
        // focus stays on the editor so the new file can be typed into
        assert_eq!(app.focus, Focus::Editor);
        assert!(app.message.is_some());

        // typing lands in the new buffer
        app.handle_key(char_key('x'));
        assert_eq!(app.buffer.lines, vec!["x"]);
    }

    #[test]
    fn ctrl_n_from_a_clean_untitled_buffer_resets_it() {
        let dir = scratch("newn2");
        let mut app = new_app(dir, None).unwrap();
        app.handle_key(ctrl('o'));
        app.handle_key(char_key('x'));
        app.handle_key(ctrl('z')); // undo -> clean again
        assert!(!app.buffer.dirty);

        app.handle_key(ctrl('n'));
        assert_eq!(app.buffer.lines, vec![""]);
        assert!(!app.buffer.dirty);
        assert_eq!(app.focus, Focus::Editor);
    }

    #[test]
    fn ctrl_n_refuses_when_the_buffer_is_dirty() {
        let dir = scratch("newn3");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        app.handle_key(char_key('X'));
        assert!(app.buffer.dirty);

        app.handle_key(ctrl('n'));
        assert_eq!(app.buffer.lines, vec!["Xalpha"]); // untouched
        assert_eq!(
            app.buffer.path.as_ref().unwrap().file_name().unwrap(),
            "a.txt"
        );
        assert!(app.message.is_some());
    }

    #[test]
    fn ctrl_n_while_previewing_starts_a_new_buffer() {
        let dir = scratch("newn4");
        write_test_png(&dir.join("pic.png"));
        let mut app = new_app(dir.clone(), Some(dir.join("pic.png"))).unwrap();
        assert!(app.image.is_some());

        app.handle_key(ctrl('n'));
        assert!(app.image.is_none());
        assert_eq!(app.buffer.lines, vec![""]);
        assert!(!app.buffer.dirty);
        assert_eq!(app.focus, Focus::Editor);
    }

    #[test]
    fn ctrl_n_in_the_save_as_prompt_does_nothing() {
        let dir = scratch("newn5");
        let mut app = new_app(dir, None).unwrap();
        app.handle_key(ctrl('o'));
        app.handle_key(char_key('x'));
        app.handle_key(ctrl('s'));
        assert!(app.save_as_input.is_some());
        app.handle_key(char_key('a'));

        app.handle_key(ctrl('n'));
        // the prompt and its text survive, the buffer is untouched
        assert!(app.save_as_input.is_some());
        assert_eq!(app.save_as_input.as_deref(), Some("a"));
        assert_eq!(app.buffer.lines, vec!["x"]);
    }

    // ---- search ------------------------------------------------------------

    fn open_search_typed(app: &mut App, query: &str) {
        app.handle_key(ctrl('f'));
        for c in query.chars() {
            app.handle_key(char_key(c));
        }
    }

    #[test]
    fn ctrl_f_opens_search_and_typing_highlights_matches() {
        let dir = scratch("search1");
        let file = dir.join("a.txt");
        fs::write(&file, "hello world\nhello again\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();

        app.handle_key(ctrl('f'));
        assert!(app.search.is_some());
        assert_eq!(app.search.as_ref().unwrap().query, "");

        open_search_typed(&mut app, "hello");
        let search = app.search.as_ref().unwrap();
        assert_eq!(search.query, "hello");
        assert_eq!(search.match_count(), 2);
        assert_eq!(search.current_index(), 0);
        // the cursor jumped to the current match...
        assert_eq!(app.buffer.cursor, (0, 0));
        // ...and the buffer itself was not edited
        assert_eq!(app.buffer.lines, vec!["hello world", "hello again", ""]);
    }

    #[test]
    fn search_enter_and_shift_enter_step_through_matches() {
        let dir = scratch("search2");
        let file = dir.join("a.txt");
        fs::write(&file, "aa bb aa\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        open_search_typed(&mut app, "aa");

        app.handle_key(key(KeyCode::Enter));
        let search = app.search.as_ref().unwrap();
        assert_eq!(search.current_index(), 1);
        assert_eq!(app.buffer.cursor, (6, 0));

        // wraps around past the last match
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.buffer.cursor, (0, 0));

        // Shift+Enter goes back (kitty terminals report it as Enter+SHIFT)
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.buffer.cursor, (6, 0));

        // Ctrl+F again jumps to the next match too
        app.handle_key(ctrl('f'));
        assert_eq!(app.buffer.cursor, (0, 0));
        assert!(app.search.is_some());
    }

    #[test]
    fn search_backspace_and_esc() {
        let dir = scratch("search3");
        let file = dir.join("a.txt");
        fs::write(&file, "foo bar\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        open_search_typed(&mut app, "foob");
        assert_eq!(app.search.as_ref().unwrap().match_count(), 0);

        app.handle_key(key(KeyCode::Backspace)); // "foo" now matches
        let search = app.search.as_ref().unwrap();
        assert_eq!(search.query, "foo");
        assert_eq!(search.match_count(), 1);
        assert_eq!(
            search.current_match(),
            Some(crate::search::Match {
                line: 0,
                start: 0,
                end: 3
            })
        );

        app.handle_key(key(KeyCode::Esc));
        assert!(app.search.is_none());
    }

    #[test]
    fn editor_navigation_dismisses_search() {
        let dir = scratch("search-navigation");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha\nbeta\ncharlie\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();

        open_search_typed(&mut app, "charlie");
        assert_eq!(app.buffer.cursor, (0, 2));
        assert!(app.search.is_some());

        // Search stays open after matching, but an editor navigation key must
        // not be swallowed by the search prompt.
        app.handle_key(key(KeyCode::Up));
        assert!(app.search.is_none());
        assert_eq!(app.buffer.cursor, (0, 1));
    }

    #[test]
    fn wrapped_search_click_then_up_continues_moving() {
        let dir = scratch("search-wrapped-click");
        let file = dir.join("README.md");
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md");
        let contents = fs::read_to_string(source).unwrap();
        fs::write(&file, &contents).unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();

        render_sized(&mut app, 60, 24); // establish the viewport width
        app.handle_key(ctrl('w'));
        render_sized(&mut app, 60, 24); // establish the wrapped rows
        open_search_typed(&mut app, "command");
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.buffer.cursor, (21, 28));

        // Redraw after the first move, then click the only search result as
        // a user would. The result starts at char 22 on README.md line 29.
        render_sized(&mut app, 60, 24);
        let target = (25, 28); // three characters into "command"
        let vrow = {
            let old = app.buffer.cursor;
            app.buffer.cursor = target;
            let row = app.buffer.cursor_vrow();
            app.buffer.cursor = old;
            row
        };
        let inner_x = app.editor_area.x as usize + 1;
        let inner_y = app.editor_area.y as usize + 1;
        let gutter_w = app.buffer.lines.len().to_string().len() + 1;
        let (chunk_start, _) = crate::buffer::visual_chunk(
            &app.buffer.lines[target.1],
            vrow - app.buffer.lines[..target.1]
                .iter()
                .map(|line| crate::buffer::visual_len(line, app.buffer.wrap_width))
                .sum::<usize>(),
            app.buffer.wrap_width,
        );
        let click_x = inner_x + gutter_w + target.0 - chunk_start;
        let click_y = inner_y + vrow - app.buffer.scroll.1;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            click_x as u16,
            click_y as u16,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            click_x as u16,
            click_y as u16,
        ));
        assert_eq!(app.buffer.cursor, target);

        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.buffer.cursor, (21, 28));
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.buffer.cursor.1, 27);
    }

    #[test]
    fn search_without_matches_keeps_cursor_put() {
        let dir = scratch("search4");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha beta\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        app.buffer.cursor = (3, 0);
        open_search_typed(&mut app, "zzz");
        let search = app.search.as_ref().unwrap();
        assert_eq!(search.match_count(), 0);
        assert_eq!(app.buffer.cursor, (3, 0));
        // the status bar shows "no matches"
        let rows = render(&mut app);
        assert!(row_contains(&rows, "no matches"));
    }

    #[test]
    fn search_rendering_highlights_matches_and_counts() {
        let dir = scratch("search5");
        let file = dir.join("a.txt");
        fs::write(&file, "hello world\nhello again\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app); // set the viewport first
        open_search_typed(&mut app, "hello");

        let buf = render_buffer(&mut app);
        // current match (line 0, chars 0..5): yellow background
        for x in 31..36 {
            assert_eq!(
                buf.cell((x, 3)).unwrap().style().bg,
                Some(SEARCH_CURRENT_BG),
                "col {x}"
            );
        }
        // the other match (line 1, chars 0..5): the dim match color
        for x in 31..36 {
            assert_eq!(
                buf.cell((x, 4)).unwrap().style().bg,
                Some(SEARCH_OTHER_BG),
                "col {x}"
            );
        }
        // outside the matches: untouched (the theme's base background,
        // like every plain cell)
        assert_eq!(buf.cell((36, 3)).unwrap().style().bg, Some(PALETTE.bg));
        assert_eq!(buf.cell((31, 5)).unwrap().style().bg, Some(PALETTE.bg));

        // the status bar shows the prompt, the query and the counter
        let rows = render(&mut app);
        assert!(row_contains(&rows, "search:"));
        assert!(row_contains(&rows, "hello"));
        assert!(row_contains(&rows, "1/2"));
    }

    #[test]
    fn mouse_click_dismisses_search() {
        let dir = scratch("search6");
        let file = dir.join("a.txt");
        fs::write(&file, "hello world\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);
        open_search_typed(&mut app, "hello");
        assert!(app.search.is_some());

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 35, 2));
        assert!(app.search.is_none());
        // and the click still moves the cursor (char 4 of line 0)
        assert_eq!(app.buffer.cursor, (4, 0));
    }

    #[test]
    fn ctrl_v_pastes_into_the_search_query() {
        let dir = scratch("search7");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha\n").unwrap();
        let mut app = with_fake_clipboard(new_app(dir, Some(file)).unwrap());
        app.clipboard.set_text("alp");
        app.handle_key(ctrl('f'));
        app.handle_key(ctrl('v'));
        assert_eq!(app.search.as_ref().unwrap().query, "alp");
        assert_eq!(app.search.as_ref().unwrap().match_count(), 1);
        assert_eq!(app.buffer.lines, vec!["alpha", ""]); // buffer untouched
    }

    #[test]
    fn ctrl_z_while_searching_does_not_undo_the_buffer() {
        let dir = scratch("search8");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        app.handle_key(char_key('X')); // buffer now "Xalpha"
        open_search_typed(&mut app, "X");
        assert_eq!(app.buffer.lines, vec!["Xalpha", ""]);

        app.handle_key(ctrl('z'));
        assert_eq!(app.buffer.lines, vec!["Xalpha", ""]);
        assert!(app.search.is_some());
    }

    #[test]
    fn clip_ops_styles_search_matches() {
        let line = "hello world";
        let style = Style::default().fg(Color::Blue);
        let spans = clip_ops(
            line,
            &[(Some(style), 0..line.len())],
            0,
            line.chars().count(),
            None,
            &[(0, 5, false)],
        );
        let parts: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(parts, vec!["hello", " world"]);
        // match keeps the syntax fg and gains the match background
        assert_eq!(spans[0].style.bg, Some(SEARCH_OTHER_BG));
        assert_eq!(spans[0].style.fg, Some(Color::Blue));
        assert_eq!(spans[1].style.bg, None);
    }

    // ---- modified printable characters ------------------------------------

    #[test]
    fn shift_modified_char_inserts_into_buffer() {
        let dir = scratch("shiftchar");
        let file = dir.join("a.txt");
        fs::write(&file, "").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();

        // Some terminals report Shift+letter as the base key plus a SHIFT
        // modifier; the ASCII letter is folded to uppercase.
        app.handle_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        app.handle_key(shift_key(KeyCode::Char('a')));
        assert_eq!(app.buffer.lines, vec!["AA"]);
    }

    #[test]
    fn alt_modified_char_inserts_into_buffer() {
        // German macOS layout: `[`/`]` are Option+5 / Option+6 and
        // `{`/`}` are Option+8 / Option+9.
        let dir = scratch("altchar");
        let file = dir.join("a.txt");
        fs::write(&file, "").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();

        app.handle_key(alt('['));
        app.handle_key(alt(']'));
        app.handle_key(alt('{'));
        app.handle_key(alt('}'));
        assert_eq!(app.buffer.lines, vec!["[]{}"]);
    }

    #[test]
    fn alt_shift_modified_char_inserts_into_buffer() {
        // e.g. Option+Shift+7 on a German macOS layout produces `|`.
        let dir = scratch("altshiftchar");
        let file = dir.join("a.txt");
        fs::write(&file, "").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();

        app.handle_key(KeyEvent::new(
            KeyCode::Char('|'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ));
        assert_eq!(app.buffer.lines, vec!["|"]);
    }

    #[test]
    fn plain_text_chars_insert_as_reported() {
        // What a kitty-protocol terminal (e.g. Ghostty on macOS) sends for
        // ordinary printable keys once REPORT_ALL_KEYS_AS_ESCAPE_CODES is
        // not requested: the resulting character as plain text, with no
        // modifiers. German layout: Shift+8 is `(`, Option+5 is `[`,
        // Option+8 is `{`, Option+9 is `}`, Option+L is `@`.
        let dir = scratch("plainchars");
        let file = dir.join("a.txt");
        fs::write(&file, "").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();

        for c in ['a', 'A', '(', '[', '{', '}', '@', ']', ')'] {
            app.handle_key(char_key(c));
        }
        assert_eq!(app.buffer.lines, vec!["aA([{}@])"]);
    }

    #[test]
    fn ctrl_and_super_chars_do_not_insert() {
        let dir = scratch("ctrlchar");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();

        // unbound Ctrl/Super letters are swallowed, not typed
        app.handle_key(ctrl('k'));
        app.handle_key(cmd('k'));
        assert_eq!(app.buffer.lines, vec!["alpha"]);

        // bound shortcuts don't type their letter either
        app.handle_key(ctrl('c'));
        assert_eq!(app.buffer.lines, vec!["alpha"]);
        assert_eq!(app.buffer.cursor, (0, 0));
    }

    #[test]
    fn save_as_prompt_accepts_alt_and_shift_chars() {
        let dir = scratch("saveasmod");
        let mut app = new_app(dir, None).unwrap();
        app.handle_key(ctrl('o'));
        app.handle_key(ctrl('s'));
        assert!(app.save_as_input.is_some());

        // `[` needs Option on German layouts; `:` and `a` need Shift on many
        // layouts (the `a` is folded to uppercase)
        app.handle_key(alt('['));
        app.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SHIFT));
        assert_eq!(app.save_as_input.as_deref(), Some("[:A"));
        assert_eq!(app.buffer.lines, vec![""]);
    }

    #[test]
    fn sidebar_enter_opens_file_and_switches_focus() {
        let dir = scratch("open");
        fs::write(dir.join("b.txt"), "beta").unwrap();
        let mut app = new_app(dir, None).unwrap();
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
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
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
        let mut app = new_app(dir.clone(), Some(file)).unwrap();
        app.handle_key(char_key('Z'));
        assert!(app.buffer.dirty);
        app.handle_key(ctrl('s'));
        assert!(!app.buffer.dirty);
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "Zalpha");
    }

    #[test]
    fn ctrl_q_requires_second_press_when_dirty() {
        let dir = scratch("quit");
        let mut app = new_app(dir, None).unwrap();
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
        let mut app = new_app(dir, None).unwrap();
        app.handle_key(ctrl('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn enter_auto_indents() {
        let dir = scratch("autoindent");
        fs::write(dir.join("a.txt"), "").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        for c in "    fn f() {}".chars() {
            app.handle_key(char_key(c));
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.buffer.lines, vec!["    fn f() {}", "    "]);
        assert_eq!(app.buffer.cursor, (4, 1));
    }

    #[test]
    fn tab_indents_selection_and_shift_tab_dedents() {
        let dir = scratch("tabindent");
        fs::write(dir.join("a.txt"), "").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        app.buffer.insert_multiline("    a\n    b");
        app.buffer.home();
        app.buffer.move_up(); // top-left
        app.buffer.begin_selection();
        app.buffer.move_down();
        app.buffer.end();

        // Tab with a selection block-indents instead of replacing it
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.buffer.lines, vec!["        a", "        b"]);

        // Shift+Tab (both the BackTab and the Tab+SHIFT encodings) dedents
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.buffer.lines, vec!["    a", "    b"]);
        app.handle_key(shift_key(KeyCode::Tab));
        assert_eq!(app.buffer.lines, vec!["a", "b"]);
    }

    #[test]
    fn sidebar_backspace_ascends() {
        let dir = scratch("ascend");
        fs::create_dir(dir.join("sub")).unwrap();
        let mut app = new_app(dir.clone(), None).unwrap();
        app.sidebar.select_name("sub");
        app.handle_key(key(KeyCode::Enter)); // descend
        assert_eq!(app.sidebar.dir, dir.join("sub"));
        app.handle_key(key(KeyCode::Backspace)); // ascend
        assert_eq!(app.sidebar.dir, dir);
    }

    #[test]
    fn sidebar_arrow_keys_open_and_ascend() {
        let dir = scratch("arrows");
        fs::create_dir(dir.join("sub")).unwrap();
        fs::write(dir.join("sub").join("f.txt"), "x").unwrap();
        let mut app = new_app(dir.clone(), None).unwrap();

        // → enters the selected directory, ← goes back up
        app.sidebar.select_name("sub");
        app.handle_key(key(KeyCode::Right)); // descend
        assert_eq!(app.sidebar.dir, dir.join("sub"));
        app.handle_key(key(KeyCode::Left)); // ascend
        assert_eq!(app.sidebar.dir, dir);

        // → on a file opens it in the editor (focus moves)
        app.sidebar.select_name("sub");
        app.handle_key(key(KeyCode::Right));
        app.sidebar.select_name("f.txt");
        app.handle_key(key(KeyCode::Right));
        assert_eq!(app.buffer.path, Some(dir.join("sub").join("f.txt")));
        assert_eq!(app.focus, Focus::Editor);
    }

    // ---- headless rendering via ratatui's TestBackend --------------------

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn uses_catppuccin_mocha_theme() {
        assert_eq!(THEME.name, ThemeName::CatppuccinMocha);
        assert_eq!(PALETTE.bg, Color::Rgb(30, 30, 46));
        assert_eq!(PALETTE.accent, Color::Rgb(137, 180, 250));
        assert_eq!(PALETTE.selection, Color::Rgb(49, 50, 68));
    }

    #[test]
    fn focused_panels_use_theme_accent() {
        let dir = scratch("focus-color");
        let file = dir.join("notes.txt");
        fs::write(&file, "hello\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();

        let buf = render_buffer(&mut app);
        assert_eq!(
            buf.cell((app.editor_area.x, app.editor_area.y + 1))
                .unwrap()
                .style()
                .fg,
            Some(FOCUS_COLOR)
        );

        app.focus = Focus::Sidebar;
        let buf = render_buffer(&mut app);
        assert_eq!(
            buf.cell((app.sidebar_area.x, app.sidebar_area.y + 1))
                .unwrap()
                .style()
                .fg,
            Some(FOCUS_COLOR)
        );
    }

    fn render(app: &mut App) -> Vec<String> {
        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(150)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect()
    }

    fn row_contains(rows: &[String], needle: &str) -> bool {
        rows.iter().any(|r| r.contains(needle))
    }

    #[test]
    fn focused_sidebar_hides_native_cursor() {
        let dir = scratch("sidebar-cursor");
        fs::write(dir.join("notes.txt"), "hello world").unwrap();
        let mut app = new_app(dir, None).unwrap();
        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| app.draw(f)).unwrap();

        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn renders_sidebar_editor_and_status() {
        let dir = scratch("render1");
        fs::create_dir(dir.join("docs")).unwrap();
        fs::write(dir.join("notes.txt"), "hello world").unwrap();
        let mut app = new_app(dir.clone(), None).unwrap();

        let rows = render(&mut app);
        // sidebar: dirs and files listed, selected row marked
        assert!(row_contains(&rows, "docs/"));
        assert!(row_contains(&rows, "notes.txt"));
        assert!(row_contains(&rows, "▶"));
        // editor: untitled block, empty buffer with line number 1
        assert!(row_contains(&rows, "untitled"));
        assert!(row_contains(&rows, "1 "));
        // top bar: the clickable shortcut buttons
        assert!(row_contains(&rows, "Ctrl+S save"));
        assert!(row_contains(&rows, "Ctrl+Shift+Z redo"));
        assert!(row_contains(&rows, "Ctrl+Q quit"));
        // status bar: focus tag + position; the shortcuts moved to the top
        assert!(row_contains(&rows, "SIDEBAR"));
        assert!(row_contains(&rows, "○ saved"));
        assert!(row_contains(&rows, "1:1"));
        assert!(!rows[23].contains("Ctrl+S"));
    }

    #[test]
    fn renders_file_content_and_modified_state() {
        let dir = scratch("render2");
        let file = dir.join("notes.txt");
        fs::write(&file, "hello world").unwrap();
        let mut app = new_app(dir.clone(), Some(file)).unwrap();

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
        let backend = TestBackend::new(150, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn render_sized(app: &mut App, width: u16, height: u16) {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
    }

    #[test]
    fn renders_syntax_highlighted_code() {
        let dir = scratch("hlrender");
        let file = dir.join("code.rs");
        fs::write(&file, "fn main() {\n    let msg = \"hi\";\n}\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);
        // "fn" keyword: mauve; "main" function name: blue
        // (colors probed from the Catppuccin Mocha theme)
        assert_eq!(buf.cell((31, 3)).unwrap().symbol(), "f");
        assert_eq!(
            buf.cell((31, 3)).unwrap().style().fg,
            Some(Color::Rgb(203, 166, 247))
        );
        assert_eq!(buf.cell((34, 3)).unwrap().symbol(), "m");
        assert_eq!(
            buf.cell((34, 3)).unwrap().style().fg,
            Some(Color::Rgb(137, 180, 250))
        );
        // string content "hi": green
        assert_eq!(buf.cell((46, 4)).unwrap().symbol(), "h");
        assert_eq!(
            buf.cell((46, 4)).unwrap().style().fg,
            Some(Color::Rgb(166, 227, 161))
        );
        // punctuation uses Catppuccin's muted overlay color
        assert_eq!(buf.cell((31, 5)).unwrap().symbol(), "}");
        assert_eq!(
            buf.cell((31, 5)).unwrap().style().fg,
            Some(Color::Rgb(147, 153, 178))
        );
    }

    #[test]
    fn caret_has_distinct_rendered_style() {
        let dir = scratch("caretstyle");
        let file = dir.join("notes.txt");
        fs::write(&file, "test\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);
        let caret = buf.cell((31, 3)).unwrap();
        assert_eq!(caret.symbol(), "t");
        assert_eq!(caret.style().bg, Some(PALETTE.warning));
        assert!(!caret.style().add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn plain_text_files_render_uncolored() {
        let dir = scratch("hlplain");
        let file = dir.join("notes.txt");
        fs::write(&file, "just some words\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);
        for x in 31..99 {
            // exclude the accent-colored focus border at x=99
            let cell = buf.cell((x, 3)).unwrap();
            if cell.symbol().is_empty() || cell.symbol() == " " {
                continue;
            }
            assert_eq!(cell.style().fg, Some(Color::Reset), "col {x}");
        }
    }

    #[test]
    fn renders_tab_indentation_in_plain_text_files() {
        let dir = scratch("tabindent-render");
        let file = dir.join("actions.coffee");
        fs::write(
            &file,
            "GLOBALS.actions =\n\t\"conversation\": (focused) ->\n",
        )
        .unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);

        // Ratatui skips literal tab control characters. The editor keeps the
        // tab in its buffer, but expands it for display so unsupported files
        // retain their indentation.
        for x in 31..35 {
            assert_eq!(buf.cell((x, 4)).unwrap().symbol(), " ", "column {x}");
        }
        assert_eq!(buf.cell((35, 4)).unwrap().symbol(), "\"");

        // The logical cursor is still before the tab. Its one-cell caret
        // must use a printable placeholder too, rather than a raw tab.
        app.buffer.cursor = (0, 1);
        let buf = render_buffer(&mut app);
        assert_eq!(buf.cell((31, 4)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((31, 4)).unwrap().style().bg, Some(PALETTE.warning));
    }

    #[test]
    fn editing_rehighlights_immediately() {
        let dir = scratch("hlrehighlight");
        let file = dir.join("code.rs");
        fs::write(&file, "fn main() {\n}\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);
        assert_ne!(buf.cell((31, 2)).unwrap().style().fg, Some(Color::Reset));
        // typing 'x' in front of "fn" must immediately re-highlight:
        // "xfn" is no longer a keyword
        app.handle_key(char_key('x'));
        let buf = render_buffer(&mut app);
        assert_eq!(buf.cell((31, 3)).unwrap().symbol(), "x");
        assert_eq!(buf.cell((31, 3)).unwrap().style().fg, Some(Color::Reset));
        assert_eq!(buf.cell((32, 3)).unwrap().symbol(), "f");
    }

    #[test]
    fn status_bar_shows_detected_syntax() {
        let dir = scratch("hlsyntax");
        let file = dir.join("code.rs");
        fs::write(&file, "fn main() {}\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        let rows = render(&mut app);
        assert!(row_contains(&rows, "[Rust] ○ saved"));

        let dir = scratch("hlsyntax2");
        let file = dir.join("notes.txt");
        fs::write(&file, "hello\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        let rows = render(&mut app);
        assert!(!row_contains(&rows, "[Plain Text]"));
    }

    #[test]
    fn renders_save_as_prompt() {
        let dir = scratch("render3");
        let mut app = new_app(dir, None).unwrap();
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
        let mut app = new_app(dir.clone(), Some(file)).unwrap();

        // cursor at the end of the buffer; drawing must scroll it into view
        // without panicking
        app.buffer.cursor = (200, 199);
        let rows = render(&mut app);
        assert!(row_contains(&rows, "line 199"));
        assert!(!row_contains(&rows, "line 000"));
    }

    #[test]
    fn renders_editor_scrollbar_for_long_content() {
        let dir = scratch("render-scrollbar");
        let file = dir.join("long.txt");
        let content: String = (0..200).map(|i| format!("line {i:03}\n")).collect();
        fs::write(&file, content).unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);
        let scrollbar_x = app.editor_area.x + app.editor_area.width - 1;
        let mut scrollbar_range =
            app.editor_area.y + 1..app.editor_area.y + app.editor_area.height - 1;
        let scrollbar = scrollbar_range
            .clone()
            .map(|y| buf.cell((scrollbar_x, y)).unwrap().symbol())
            .collect::<String>();
        assert!(scrollbar.contains('█'));
        assert!(scrollbar.contains('│'));
        let thumb_y = scrollbar_range
            .find(|&y| buf.cell((scrollbar_x, y)).unwrap().symbol() == "█")
            .unwrap();
        assert_eq!(
            buf.cell((scrollbar_x, thumb_y)).unwrap().style().fg,
            Some(FOCUS_COLOR)
        );

        // At the final viewport the thumb reaches the bottom of the track.
        app.buffer.scroll.1 = app
            .buffer
            .total_visual_rows()
            .saturating_sub(app.editor_text.1 as usize);
        let buf = render_buffer(&mut app);
        let bottom = app.editor_area.y + app.editor_area.height - 2;
        assert_eq!(buf.cell((scrollbar_x, bottom)).unwrap().symbol(), "█");
        assert_eq!(
            buf.cell((scrollbar_x, bottom)).unwrap().style().fg,
            Some(FOCUS_COLOR)
        );
    }

    // ---- wrapping ----------------------------------------------------------

    #[test]
    fn ctrl_w_toggles_wrap() {
        let dir = scratch("wrap1");
        let file = dir.join("a.txt");
        fs::write(&file, "some content\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        assert!(!app.buffer.wrap);

        app.handle_key(ctrl('w'));
        assert!(app.buffer.wrap);
        assert!(app.message.is_some());

        app.handle_key(ctrl('w'));
        assert!(!app.buffer.wrap);
    }

    #[test]
    fn wrapped_long_line_renders_across_rows() {
        let dir = scratch("wrap2");
        let file = dir.join("a.txt");
        // 150-col test terminal: 28 sidebar + 2 borders -> 118 text cols
        let line = "x".repeat(250);
        fs::write(&file, format!("{line}\n")).unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        app.handle_key(ctrl('w'));
        app.message = None; // let the wrap indicator show
        let rows = render(&mut app);

        // the line wraps onto three visual rows: the line number only on
        // the first, a blank gutter on the continuations (cols 29..31 are
        // the editor gutter; border chars are multi-byte, so slice chars)
        let gutter = |row: &str| -> String { row.chars().skip(29).take(2).collect() };
        assert_eq!(gutter(&rows[3]), "1 ");
        assert_eq!(gutter(&rows[4]), "  ");
        assert_eq!(gutter(&rows[5]), "  ");
        assert!(row_contains(&rows, &"x".repeat(118)));
        // the status bar shows the persistent wrap indicator
        assert!(rows[23].contains("wrap ○"));

        // back to no wrapping: one line per row, no indicator
        app.handle_key(ctrl('w'));
        app.message = None;
        let rows = render(&mut app);
        assert_eq!(gutter(&rows[3]), "1 ");
        assert_eq!(gutter(&rows[4]), "2 ");
        assert!(!rows[23].contains("wrap ○"));
    }

    #[test]
    fn wrapped_mouse_click_maps_to_visual_rows() {
        let dir = scratch("wrap3");
        let file = dir.join("a.txt");
        let line = "x".repeat(250);
        fs::write(&file, format!("{line}\n")).unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        app.handle_key(ctrl('w'));
        render_buffer(&mut app); // sets viewport and wrap width

        // terminal col 36 = text col 5 on the second visual row (char 123)
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 36, 4));
        assert_eq!(app.buffer.cursor, (123, 0));

        // gutter of the third visual row lands at its start (char 236)
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 5));
        assert_eq!(app.buffer.cursor, (236, 0));
    }

    #[test]
    fn wrapped_caret_renders_on_the_cursor_row() {
        let dir = scratch("wrap4");
        let file = dir.join("a.txt");
        let line = "x".repeat(250);
        fs::write(&file, format!("{line}\n")).unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        app.handle_key(ctrl('w'));
        render_buffer(&mut app);

        // cursor on the first visual row: char 110 is col 110 of it
        app.buffer.cursor = (110, 0);
        let buf = render_buffer(&mut app);
        let cell = buf.cell((141, 3)).unwrap();
        assert_eq!(cell.symbol(), "x");
        assert_eq!(cell.style().bg, Some(PALETTE.warning));
    }

    #[test]
    fn wrapped_text_breaks_at_words_in_rendering() {
        let dir = scratch("wrap5");
        let file = dir.join("a.txt");
        // 30 words of 6 chars (179 chars); at 118 text cols a row
        // holds 19 words and wraps after a space
        let line = "hello ".repeat(30).trim_end().to_string();
        fs::write(&file, format!("{line}\n")).unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        app.handle_key(ctrl('w'));
        app.message = None;
        let rows = render(&mut app);
        let text = |row: &str| -> String { row.chars().skip(31).take(118).collect() };
        let gutter = |row: &str| -> String { row.chars().skip(29).take(2).collect() };

        // first row ends exactly at the wrap point (after a space); the
        // rest of the 118-cell row is blank
        assert_eq!(
            text(&rows[3]).trim_end(),
            format!("{}hello", "hello ".repeat(18))
        );
        // second row continues with whole words and a blank gutter
        assert_eq!(gutter(&rows[4]), "  ");
        assert_eq!(
            text(&rows[4]).trim_end(),
            format!("{}hello", "hello ".repeat(10))
        );
    }

    // ---- mouse -------------------------------------------------------------

    #[test]
    fn mouse_click_positions_cursor_and_focuses_editor() {
        let dir = scratch("mclick");
        let file = dir.join("code.rs");
        fs::write(&file, "fn main() {\n    let x = 1;\n}\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app); // sets widget areas

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 35, 4));
        assert_eq!(app.focus, Focus::Editor);
        // terminal col 35 = text col 4 on line 2 (index 1)
        assert_eq!(app.buffer.cursor, (4, 1));

        // clicking in the gutter lands at column 0
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 29, 5));
        assert_eq!(app.buffer.cursor, (0, 2));
    }

    #[test]
    fn mouse_drag_selects_text() {
        let dir = scratch("mdrag");
        let file = dir.join("code.rs");
        fs::write(&file, "fn main() {\n}\n").unwrap();
        let mut app = with_fake_clipboard(new_app(dir, Some(file)).unwrap());
        render_buffer(&mut app);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 2));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 33, 2));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 33, 2));
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
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 33, 2));
        let shift_click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 36,
            row: 2,
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
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        // terminal col 37 = char 6 = 'b' of "brave"
        let click = |kind| mouse(kind, 37, 2);
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
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        let click = |kind| mouse(kind, 31, 2); // char 0 of line 0
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
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        let click = |kind| mouse(kind, 37, 2); // char 6 = 'b' of "brave"
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
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        let click = |kind| mouse(kind, 37, 2); // char 6 = 'b' of "brave"
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        // drag into "world" (char 16) -> extends word-wise
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 47, 2));
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
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        let click = |kind| mouse(kind, 37, 2); // char 6 = 'b' of "brave"
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        // hand jitter while holding the second click: still inside "brave"
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 39, 2));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        assert_eq!(app.buffer.selected_text().as_deref(), Some("brave"));
    }

    #[test]
    fn drag_after_triple_click_extends_by_line() {
        let dir = scratch("mtripledrag");
        let file = dir.join("a.txt");
        fs::write(&file, "one two three\nfour five\nsix\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        let click = |kind| mouse(kind, 31, 3); // char 0 of line 0
        for _ in 0..3 {
            app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
            app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        }
        // drag to line 2 (terminal row 5) -> extends line-wise
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 31, 5));
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
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        // double-click "brave" -> word selected
        let click = |kind| mouse(kind, 37, 2);
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        assert_eq!(app.buffer.selected_text().as_deref(), Some("brave"));

        // clicking a different position right away is a plain click again
        let other = mouse(MouseEventKind::Down(MouseButton::Left), 31, 2);
        app.handle_mouse(other);
        assert!(!app.buffer.has_selection());
        assert_eq!(app.buffer.cursor, (0, 0));
    }

    #[test]
    fn double_click_on_whitespace_selects_whitespace() {
        let dir = scratch("mdblspace");
        let file = dir.join("a.txt");
        fs::write(&file, "hello world\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        // terminal col 36 = char 5 = the space between the words
        let click = |kind| mouse(kind, 36, 2);
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
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        assert_eq!(app.buffer.cursor, (0, 0));
        assert_eq!(app.buffer.scroll.1, 0);
        app.handle_mouse(mouse(MouseEventKind::ScrollDown, 60, 10));
        assert_eq!(app.buffer.cursor, (0, 0));
        assert_eq!(app.buffer.scroll.1, 3);
        let rows = render(&mut app);
        assert!(row_contains(&rows, "line 3"));
        assert!(!row_contains(&rows, "line 0"));
        app.handle_mouse(mouse(MouseEventKind::ScrollUp, 60, 10));
        assert_eq!(app.buffer.cursor, (0, 0));
        assert_eq!(app.buffer.scroll.1, 0);

        // wheel over the sidebar moves the selection
        app.handle_mouse(mouse(MouseEventKind::ScrollDown, 5, 5));
        assert_eq!(app.sidebar.selected, 1);

        // Horizontal wheel events over the sidebar must not scroll the editor.
        // The editor is already at the left edge, so a rightward event makes
        // the regression visible without depending on the file contents.
        assert_eq!(app.buffer.scroll.0, 0);
        app.handle_mouse(mouse(MouseEventKind::ScrollRight, 5, 5));
        assert_eq!(app.buffer.scroll.0, 0);

        // The same event still scrolls when it is actually over the editor.
        app.handle_mouse(mouse(MouseEventKind::ScrollRight, 60, 10));
        assert_eq!(app.buffer.scroll.0, 3);
        app.handle_mouse(mouse(MouseEventKind::ScrollLeft, 60, 10));
        assert_eq!(app.buffer.scroll.0, 0);
    }

    #[test]
    fn sidebar_single_click_selects_double_click_opens() {
        let dir = scratch("mdblclick");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = new_app(dir, None).unwrap();
        render_buffer(&mut app);

        // single click on the file row (row 1: ".." is row 0)
        let click = |kind| mouse(kind, 5, 4);
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
        let mut app = new_app(dir.clone(), None).unwrap();
        render_buffer(&mut app);

        let click = |kind| mouse(kind, 5, 3); // row 0: ".."
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
        // opened the parent of the scratch dir
        assert_ne!(app.sidebar.dir, dir);
    }

    // ---- shortcut buttons in the top bar --------------------------------

    /// Click the button for `action` (built by the last draw) and return
    /// the `App` for further assertions.
    fn click_button(app: &mut App, action: Shortcut) {
        let (_, rect) = app
            .topbar_buttons
            .iter()
            .find(|(a, _)| *a == action)
            .unwrap_or_else(|| panic!("no {action:?} button in the top bar"));
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + rect.width / 2,
            rect.y,
        ));
    }

    #[test]
    fn clicking_save_button_saves_the_file() {
        let dir = scratch("mbtnsave");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha").unwrap();
        let mut app = new_app(dir.clone(), Some(file)).unwrap();
        render_buffer(&mut app); // builds the button rectangles

        app.handle_key(char_key('X'));
        assert!(app.buffer.dirty);
        click_button(&mut app, Shortcut::Save);
        assert!(!app.buffer.dirty);
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "Xalpha");
    }

    #[test]
    fn clicking_switch_button_toggles_focus() {
        let dir = scratch("mbtnswitch");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        render_buffer(&mut app);

        assert_eq!(app.focus, Focus::Editor);
        click_button(&mut app, Shortcut::SwitchFocus);
        assert_eq!(app.focus, Focus::Sidebar);
        click_button(&mut app, Shortcut::SwitchFocus);
        assert_eq!(app.focus, Focus::Editor);
    }

    #[test]
    fn clicking_undo_and_redo_buttons_work() {
        let dir = scratch("mbtnundo");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        app.handle_key(char_key('X'));
        assert_eq!(app.buffer.lines, vec!["Xalpha"]);
        click_button(&mut app, Shortcut::Undo);
        assert_eq!(app.buffer.lines, vec!["alpha"]);
        click_button(&mut app, Shortcut::Redo);
        assert_eq!(app.buffer.lines, vec!["Xalpha"]);
    }

    #[test]
    fn clicking_quit_button_requires_two_clicks_when_dirty() {
        let dir = scratch("mbtnquit");
        let mut app = new_app(dir, None).unwrap();
        app.handle_key(ctrl('o'));
        app.handle_key(char_key('x'));
        render_buffer(&mut app);

        click_button(&mut app, Shortcut::Quit);
        assert!(!app.should_quit); // armed, not quit
        click_button(&mut app, Shortcut::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn clicking_quit_button_quits_when_clean() {
        let dir = scratch("mbtnquitclean");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        render_buffer(&mut app);

        click_button(&mut app, Shortcut::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn clicking_search_button_opens_search() {
        let dir = scratch("mbtnsearch");
        fs::write(dir.join("a.txt"), "hello world\nhello again\n").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        render_buffer(&mut app);

        click_button(&mut app, Shortcut::Find);
        assert!(app.search.is_some());
        // typing narrows the query; clicking again steps to the next
        // match, exactly like Ctrl+F
        for c in "hello".chars() {
            app.handle_key(char_key(c));
        }
        assert_eq!(app.buffer.cursor, (0, 0)); // first match
        click_button(&mut app, Shortcut::Find);
        assert_eq!(app.buffer.cursor, (0, 1)); // second match
    }

    #[test]
    fn clicking_wrap_button_toggles_wrapping() {
        let dir = scratch("mbtnwrapbtn");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        render_buffer(&mut app);
        assert!(!app.buffer.wrap);
        click_button(&mut app, Shortcut::ToggleWrap);
        assert!(app.buffer.wrap);
        assert!(app.message.is_some());
        click_button(&mut app, Shortcut::ToggleWrap);
        assert!(!app.buffer.wrap);
    }

    #[test]
    fn clicking_top_bar_gap_does_nothing() {
        let dir = scratch("mbtngap");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        render_buffer(&mut app);
        assert_eq!(app.focus, Focus::Editor);

        // after the last button there is empty bar; clicking it must not
        // move focus, arm quit, or do anything else
        let end = app
            .topbar_buttons
            .iter()
            .map(|(_, r)| r.x + r.width)
            .max()
            .unwrap();
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), end + 2, 0));
        assert_eq!(app.focus, Focus::Editor);
        assert!(!app.should_quit);
        assert!(!app.buffer.dirty);
    }

    #[test]
    fn hovering_top_bar_button_describes_it_in_the_status_bar() {
        let dir = scratch("mbtnhover");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        render_buffer(&mut app);
        let (_, rect) = app
            .topbar_buttons
            .iter()
            .find(|(a, _)| *a == Shortcut::Save)
            .unwrap();

        // moving the mouse over the button shows its description...
        app.handle_mouse(mouse(MouseEventKind::Moved, rect.x + 1, rect.y));
        let rows = render(&mut app);
        assert!(rows[23].contains("save the current file"));

        // ...and moving away restores the cursor position
        app.handle_mouse(mouse(MouseEventKind::Moved, 139, 10));
        let rows = render(&mut app);
        assert!(rows[23].contains("1:1"));
    }

    #[test]
    fn hovering_top_bar_button_highlights_it() {
        let dir = scratch("mbtnhoverbg");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        let buf = render_buffer(&mut app);
        let (_, rect) = app
            .topbar_buttons
            .iter()
            .find(|(a, _)| *a == Shortcut::Save)
            .copied()
            .unwrap();
        assert_eq!(rect.x, 30); // guard against layout drift

        // the bar starts one cell in from the window edge: cell (0,0) is
        // plain margin, not part of the first button
        assert_eq!(buf.cell((0, 0)).unwrap().style().bg, Some(PALETTE.bg));

        // moving the mouse over the button highlights it...
        app.handle_mouse(mouse(MouseEventKind::Moved, rect.x + 1, rect.y));
        let buf = render_buffer(&mut app);
        for x in rect.x..rect.x + rect.width {
            assert_eq!(
                buf.cell((x, 0)).unwrap().style().bg,
                Some(TOPBAR_PILL_BG_HOVER),
                "col {x}"
            );
        }
        // ...while a non-hovered button blends into the base background
        assert_eq!(buf.cell((1, 0)).unwrap().style().bg, Some(PALETTE.bg));

        // moving away restores the base background
        app.handle_mouse(mouse(MouseEventKind::Moved, 140, 10));
        let buf = render_buffer(&mut app);
        assert_eq!(
            buf.cell((rect.x + 1, 0)).unwrap().style().bg,
            Some(PALETTE.bg)
        );
    }

    #[test]
    fn mouse_leaving_terminal_clears_hover_highlight() {
        let dir = scratch("mbtnleave");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        render_buffer(&mut app);

        // hover the first button...
        app.handle_mouse(mouse(MouseEventKind::Moved, 2, 0));
        let buf = render_buffer(&mut app);
        assert_eq!(
            buf.cell((1, 0)).unwrap().style().bg,
            Some(TOPBAR_PILL_BG_HOVER)
        );

        // ...then the terminal reports (0,0) when the mouse leaves the
        // window: the highlight (and the status-bar description) must go
        // away instead of sticking on the first button
        app.handle_mouse(mouse(MouseEventKind::Moved, 0, 0));
        let buf = render_buffer(&mut app);
        assert_eq!(buf.cell((1, 0)).unwrap().style().bg, Some(PALETTE.bg));
        let rows = render(&mut app);
        assert!(rows[23].contains("1:1"));
    }

    #[test]
    fn top_bar_wraps_onto_a_second_row_on_narrow_terminals() {
        let dir = scratch("mbtnwrap");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();

        // 80 columns: the buttons flow onto a second row but stay visible
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let rows: Vec<String> = terminal
            .backend()
            .buffer()
            .content()
            .chunks(80)
            .map(|row| row.iter().map(|c| c.symbol()).collect())
            .collect();
        assert!(rows[0].contains("Ctrl+O switch"));
        assert!(rows[0].contains("Ctrl+N new"));
        assert!(rows[1].contains("Ctrl+C copy"));
        assert!(rows[1].contains("Ctrl+W wrap"));
        // the overflow button (Ctrl+Q) is dropped with an ellipsis marker
        assert!(rows[1].contains("…"));
        assert!(!rows[1].contains("Ctrl+Q"));
        assert_eq!(app.topbar_area.height, 2);

        // clicking a button on the second row still works
        click_button(&mut app, Shortcut::ToggleWrap);
        assert!(app.buffer.wrap);
    }

    #[test]
    fn top_bar_in_image_mode_has_preview_buttons() {
        let dir = scratch("mbtnimg");
        write_test_png(&dir.join("pic.png"));
        let mut app = new_app(dir.clone(), Some(dir.join("pic.png"))).unwrap();
        render_buffer(&mut app);
        let actions: Vec<Shortcut> = app.topbar_buttons.iter().map(|(a, _)| *a).collect();
        assert_eq!(
            actions,
            vec![
                Shortcut::ClosePreview,
                Shortcut::SwitchFocus,
                Shortcut::Quit
            ]
        );
        assert_eq!(app.topbar_area.height, 1);

        // clicking "close preview" drops back to an empty editor buffer
        click_button(&mut app, Shortcut::ClosePreview);
        assert!(app.image.is_none());
        assert_eq!(app.buffer.lines, vec![""]);

        // a second draw shows the full editing button set again
        render_buffer(&mut app);
        let actions: Vec<Shortcut> = app.topbar_buttons.iter().map(|(a, _)| *a).collect();
        assert_eq!(actions.len(), 11);
        assert_eq!(actions[0], Shortcut::SwitchFocus);
    }

    // ---- clipboard ---------------------------------------------------------

    #[test]
    fn copy_cut_paste_roundtrip() {
        let dir = scratch("clip1");
        let mut app = with_fake_clipboard(new_app(dir, None).unwrap());
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
        let mut app = with_fake_clipboard(new_app(dir, None).unwrap());
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
        let mut app = with_fake_clipboard(new_app(dir.clone(), Some(file)).unwrap());
        app.handle_key(char_key('X'));
        assert!(app.buffer.dirty);
        app.handle_key(cmd('s')); // Cmd+S saves
        assert!(!app.buffer.dirty);
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "Xalpha");

        app.handle_key(cmd('o')); // Cmd+O switches focus
        assert_eq!(app.focus, Focus::Sidebar);
        app.handle_key(cmd('o'));
        assert_eq!(app.focus, Focus::Editor);
        app.handle_key(cmd('n')); // Cmd+N starts a new untitled buffer
        assert_eq!(app.buffer.lines, vec![""]);
        assert!(app.buffer.path.is_none());
        app.handle_key(cmd('q')); // Cmd+Q quits
        assert!(app.should_quit);
    }

    #[test]
    fn bracketed_paste_inserts_multiline() {
        let dir = scratch("clip4");
        let mut app = new_app(dir, None).unwrap();
        app.handle_key(ctrl('o'));
        app.handle_key(char_key('a'));
        app.paste_text("b\nc".to_string());
        assert_eq!(app.buffer.lines, vec!["ab", "c"]);
        assert!(app.buffer.dirty);
    }

    #[test]
    fn paste_with_selection_replaces_it() {
        let dir = scratch("clip5");
        let mut app = with_fake_clipboard(new_app(dir, None).unwrap());
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
            &[],
        );

        assert_eq!(
            spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<Vec<_>>(),
            vec!["fn ", "ma", "in"]
        );
        assert_eq!(spans[0].style.bg, None);
        assert_eq!(spans[1].style.bg, Some(PALETTE.selection));
        assert_eq!(spans[2].style.bg, None);
        assert_eq!(spans[0].style.fg, Some(Color::Blue));
        assert_eq!(spans[1].style.fg, Some(Color::Blue));
        assert_eq!(spans[2].style.fg, Some(Color::Blue));
    }

    #[test]
    fn selection_byte_range_is_not_reinterpreted_as_char_range() {
        let line = "— browse directories on the left";
        let start = line.find("directories").unwrap();
        let end = start + "directories".len();
        let spans = clip_ops(
            line,
            &[(None, 0..line.len())],
            0,
            line.chars().count(),
            Some((start, end)),
            &[],
        );

        let selected: Vec<&str> = spans
            .iter()
            .filter(|span| span.style.bg == Some(PALETTE.selection))
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(selected, vec!["directories"]);
    }

    #[test]
    fn selection_renders_with_theme_background() {
        let dir = scratch("mselrender");
        let file = dir.join("a.txt");
        fs::write(&file, "hello world\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        app.buffer.home();
        app.buffer.begin_selection();
        app.buffer.end();
        let buf = render_buffer(&mut app);
        // cells inside the selection use the Catppuccin selection color
        for x in 31..42 {
            assert_eq!(
                buf.cell((x, 3)).unwrap().style().bg,
                Some(PALETTE.selection),
                "col {x}"
            );
        }
        // cells outside retain the themed base background
        assert_eq!(buf.cell((43, 3)).unwrap().style().bg, Some(PALETTE.bg));
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
        let mut app = new_app(dir.clone(), Some(file)).unwrap();

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
        let mut app = new_app(dir.clone(), Some(file)).unwrap();

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
        let mut app = new_app(dir.clone(), Some(file)).unwrap();

        app.handle_key(char_key('X'));
        app.handle_key(ctrl('o')); // switch to the sidebar
        assert_eq!(app.focus, Focus::Sidebar);
        app.handle_key(ctrl('z'));
        assert_eq!(app.buffer.lines, vec!["alpha"]);
    }

    #[test]
    fn ctrl_z_in_save_as_prompt_leaves_buffer_alone() {
        let dir = scratch("undoredo5");
        let mut app = new_app(dir, None).unwrap();
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
        let mut app = new_app(dir.clone(), Some(file)).unwrap();

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
        let mut app = new_app(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);
        assert_ne!(buf.cell((31, 2)).unwrap().style().fg, Some(Color::Reset));

        // typing breaks the keyword, undo restores both text and color
        app.handle_key(char_key('x'));
        let buf = render_buffer(&mut app);
        assert_eq!(buf.cell((31, 3)).unwrap().style().fg, Some(Color::Reset));

        app.handle_key(ctrl('z'));
        let buf = render_buffer(&mut app);
        assert_eq!(buf.cell((31, 3)).unwrap().symbol(), "f");
        assert_eq!(
            buf.cell((31, 3)).unwrap().style().fg,
            Some(Color::Rgb(203, 166, 247))
        );
    }

    #[test]
    fn top_bar_lists_undo_shortcut() {
        let dir = scratch("undoredo8");
        let mut app = new_app(dir, None).unwrap();
        let rows = render(&mut app);
        // the shortcut buttons live in the top bar now (Ctrl+Q wraps onto
        // the second row at this width)
        assert!(rows[0].contains("Ctrl+Z undo"));
        assert!(rows[0].contains("Ctrl+Shift+Z redo"));
        assert!(rows[1].contains("Ctrl+Q quit"));
    }

    // ---- image previews ----------------------------------------------------

    /// Write a 4x2 px test image: top row red, bottom row blue.
    fn write_test_png(path: &std::path::Path) {
        let mut img = image::RgbImage::new(4, 2);
        for x in 0..4 {
            img.put_pixel(x, 0, image::Rgb([255, 0, 0]));
            img.put_pixel(x, 1, image::Rgb([0, 0, 255]));
        }
        img.save(path).unwrap();
    }

    fn write_large_test_png(path: &std::path::Path) {
        let mut img = image::RgbImage::new(1200, 600);
        for y in 0..600 {
            for x in 0..1200 {
                img.put_pixel(x, y, image::Rgb([(x / 5) as u8, (y / 3) as u8, 128]));
            }
        }
        img.save(path).unwrap();
    }

    #[test]
    fn image_path_detection() {
        use crate::image_view::is_image_path;
        for name in [
            "a.png",
            "a.PNG",
            "photo.JpEg",
            "a.gif",
            "a.webp",
            "a.bmp",
            "a.jpeg",
            "a.tiff",
            "a.tif",
            "a.qoi",
            "a.Pnm",
        ] {
            assert!(is_image_path(std::path::Path::new(&name)), "{name}");
        }
        for name in ["a.txt", "a", "a.png.bak", "dir"] {
            assert!(!is_image_path(std::path::Path::new(name)), "{name}");
        }
    }

    #[test]
    fn opening_image_arg_starts_a_preview() {
        let dir = scratch("imgarg");
        let file = dir.join("pic.png");
        write_test_png(&file);
        let mut app = new_app(dir, Some(file)).unwrap();
        assert!(app.image.is_some());
        assert_eq!(app.buffer.lines, vec![""]);
        assert!(app.buffer.path.is_none());
        assert_eq!(app.focus, Focus::Editor);
        let rows = render(&mut app);
        // status bar advertises the preview: tag, dimensions, and hint
        assert!(row_contains(&rows, "IMAGE"));
        assert!(row_contains(&rows, "[4x2]"));
        assert!(row_contains(&rows, "Esc close preview"));
    }

    #[test]
    fn image_preview_title_uses_image_path() {
        let dir = scratch("imgtitle");
        let file = dir.join("pic.png");
        write_test_png(&file);
        let mut app = new_app(dir, Some(file)).unwrap();
        let rows = render(&mut app);
        assert!(row_contains(&rows, "pic.png"));
        assert!(!row_contains(&rows, "untitled"));
    }

    #[test]
    fn updating_image_metrics_invalidates_protocol_without_redecoding() {
        let dir = scratch("imgmetrics");
        let file = dir.join("pic.png");
        write_test_png(&file);
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);

        let (pixels, protocol_was_built) = {
            let preview = app.image.as_ref().unwrap();
            (preview.pixels, preview.has_cached_protocol())
        };
        assert!(protocol_was_built);

        app.update_image_metrics(Picker::halfblocks(), FontSize::new(4, 8));
        let preview = app.image.as_ref().unwrap();
        assert_eq!(preview.pixels, pixels);
        assert!(!preview.has_cached_protocol());

        // The next draw rebuilds the protocol from the existing decoded image.
        render_buffer(&mut app);
        assert!(app.image.as_ref().unwrap().has_cached_protocol());
    }

    #[test]
    fn preview_renders_halfblock_pixels() {
        let dir = scratch("imgdraw");
        let file = dir.join("pic.png");
        write_test_png(&file);
        let mut app = new_app(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);
        // the editor pane starts at column 28 (sidebar width); the 4x2 px
        // image is one cell at the assumed 10x20 font. The half-blocks
        // renderer places a colored block character there — the exact
        // color is image-rs's resize math (aspect-fit + triangle filter),
        // so assert the structure: a half-block, colored, with a red/blue
        // blend that contains no green.
        let cell = buf.cell((29, 2)).unwrap();
        assert!(matches!(cell.symbol(), "▀" | "▄"), "{:?}", cell.symbol());
        let fg = cell.style().fg;
        let Some(Color::Rgb(r, g, b)) = fg else {
            panic!("expected an RGB foreground, got {fg:?}");
        };
        assert!(r > 0 && b > 0 && g == 0, "got rgb({r},{g},{b})");
    }

    #[test]
    fn large_preview_is_contained_without_clipping() {
        let dir = scratch("imglarge");
        let file = dir.join("large.png");
        write_large_test_png(&file);
        let mut app = new_app(dir.clone(), Some(file)).unwrap();
        let buf = render_buffer(&mut app);

        // The editor's inner area is columns 29..=138. A wide image is
        // contained by height, so it must not be stretched to the right edge.
        let rendered_x: Vec<u16> = (29..=138)
            .filter(|&x| {
                matches!(
                    buf.cell((x, 2)).and_then(|c| c.style().fg),
                    Some(Color::Rgb(..))
                )
            })
            .collect();
        assert!(!rendered_x.is_empty());
        assert!(*rendered_x.iter().max().unwrap() < 138);

        // A tall image is contained by height too, and its last visible row
        // is still part of the image rather than being clipped away.
        let medium = dir.join("medium.png");
        let mut medium_img = image::RgbImage::new(1000, 400);
        for y in 0..400 {
            for x in 0..1000 {
                medium_img.put_pixel(x, y, image::Rgb([(x / 4) as u8, (y / 2) as u8, 128]));
            }
        }
        medium_img.save(&medium).unwrap();
        let mut medium_app = new_app(dir.clone(), Some(medium)).unwrap();
        let medium_buf = render_buffer(&mut medium_app);
        assert!(matches!(
            medium_buf.cell((29, 21)).and_then(|c| c.style().fg),
            Some(Color::Rgb(..))
        ));

        let tall = dir.join("tall.png");
        let mut img = image::RgbImage::new(600, 1200);
        for y in 0..1200 {
            for x in 0..600 {
                img.put_pixel(x, y, image::Rgb([(x / 3) as u8, (y / 5) as u8, 128]));
            }
        }
        img.save(&tall).unwrap();
        let mut tall_app = new_app(dir, Some(tall)).unwrap();
        let tall_buf = render_buffer(&mut tall_app);
        assert!(matches!(
            tall_buf.cell((29, 21)).and_then(|c| c.style().fg),
            Some(Color::Rgb(..))
        ));
    }

    #[test]
    fn esc_closes_preview_and_other_keys_are_ignored() {
        let dir = scratch("imgclose");
        let file = dir.join("pic.png");
        write_test_png(&file);
        let mut app = new_app(dir, Some(file)).unwrap();

        // typing, saving, and searching do nothing while previewing
        app.handle_key(char_key('a'));
        assert!(app.image.is_some());
        assert_eq!(app.buffer.lines, vec![""]);
        app.handle_key(ctrl('s'));
        assert!(app.save_as_input.is_none());
        app.handle_key(ctrl('f'));
        assert!(app.search.is_none());
        // bracketed paste is ignored too
        app.paste_text("pasted".to_string());
        assert_eq!(app.buffer.lines, vec![""]);

        // Esc closes the preview, then editing works again
        app.handle_key(key(KeyCode::Esc));
        assert!(app.image.is_none());
        app.handle_key(char_key('a'));
        assert_eq!(app.buffer.lines, vec!["a"]);
    }

    #[test]
    fn ctrl_o_and_ctrl_q_still_work_while_previewing() {
        let dir = scratch("imghotkeys");
        let file = dir.join("pic.png");
        write_test_png(&file);
        let mut app = new_app(dir, Some(file)).unwrap();
        app.handle_key(ctrl('o'));
        assert_eq!(app.focus, Focus::Sidebar);
        app.handle_key(ctrl('o'));
        assert_eq!(app.focus, Focus::Editor);
        // clean buffer: Ctrl+Q quits immediately
        app.handle_key(ctrl('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn sidebar_navigation_continues_while_preview_is_active() {
        let dir = scratch("imgnavigate");
        write_test_png(&dir.join("pic.png"));
        write_test_png(&dir.join("other.png"));
        fs::write(dir.join("notes.txt"), "text\n").unwrap();
        fs::create_dir(dir.join("nested")).unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("pic.png"))).unwrap();

        app.handle_key(ctrl('o'));
        assert_eq!(app.focus, Focus::Sidebar);
        app.sidebar.select_name("notes.txt");
        let start = app.sidebar.selected;
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.sidebar.selected, start);

        // Directory navigation remains available even though the editor is
        // displaying an image.
        app.sidebar.select_name("nested");
        app.handle_key(key(KeyCode::Enter));
        assert!(app.image.is_some());
        assert_eq!(app.focus, Focus::Sidebar);
        assert_eq!(app.sidebar.dir, dir.join("nested"));
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.sidebar.dir, dir);

        // Enter can open another image directly from the active preview.
        app.sidebar.select_name("other.png");
        app.handle_key(key(KeyCode::Enter));
        let preview = app.image.as_ref().expect("image preview");
        assert_eq!(preview.path, dir.join("other.png"));
        assert_eq!(app.focus, Focus::Editor);
    }

    #[test]
    fn sidebar_opens_images_and_dirty_buffers_block_them() {
        let dir = scratch("imgside");
        fs::write(dir.join("a.txt"), "text\n").unwrap();
        write_test_png(&dir.join("pic.png"));
        let mut app = new_app(dir, None).unwrap();
        // ".." = 0, a.txt = 1, pic.png = 2
        app.sidebar.selected = 2;
        app.handle_key(key(KeyCode::Enter));
        assert!(app.image.is_some());
        assert_eq!(app.focus, Focus::Editor);
        assert_eq!(app.buffer.lines, vec![""]);

        // a dirty buffer blocks opening an image, like any other file
        let dir = scratch("imgside2");
        fs::write(dir.join("a.txt"), "text\n").unwrap();
        write_test_png(&dir.join("pic.png"));
        let mut app = new_app(dir, None).unwrap();
        app.sidebar.selected = 1;
        app.handle_key(key(KeyCode::Enter)); // open a.txt
        app.handle_key(char_key('x')); // dirty it
        app.handle_key(ctrl('o')); // back to the sidebar
        app.sidebar.selected = 2;
        app.handle_key(key(KeyCode::Enter)); // try to open pic.png
        assert!(app.image.is_none());
        assert!(app.buffer.dirty);
    }

    #[test]
    fn corrupt_image_is_an_open_error() {
        let dir = scratch("imgcorrupt");
        let file = dir.join("bad.png");
        fs::write(&file, "definitely not an image").unwrap();
        let picker = Picker::halfblocks();
        let err = match App::new_with_cell_size(
            dir,
            Some(file),
            picker,
            image_view::fallback_logical_cell_size(),
        ) {
            Ok(_) => panic!("expected an error for a corrupt image"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("not a valid image"), "{err}");
    }
}
