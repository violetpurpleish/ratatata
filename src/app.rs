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
use crate::search::{Search, SearchField};
use crate::sidebar::{Kind, Sidebar};
use crate::theme::{self, ColorSupport};
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
/// The shortcut bar at the top wraps onto extra rows when the buttons
/// don't fit. A MacBook Air Ghostty window at 1280 logical px is ~120
/// columns, which needs three rows to keep Quit on screen with 17
/// buttons. Non-Quit overflow beyond this is marked with an ellipsis;
/// Quit is pinned and never omitted.
const TOPBAR_MAX_ROWS: u16 = 3;
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
/// Runtime drawing uses `App::pal().accent` so this alias is test-only.
#[cfg(test)]
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
    ToggleSidebar,
    Save,
    Reload,
    Copy,
    Cut,
    Paste,
    SelectAll,
    Find,
    Replace,
    GoToLine,
    ToggleWrap,
    ToggleHidden,
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
            Shortcut::ToggleSidebar => "Ctrl+B",
            Shortcut::Save => "Ctrl+S",
            Shortcut::Reload => "Ctrl+R",
            Shortcut::Copy => "Ctrl+C",

            Shortcut::Cut => "Ctrl+X",
            Shortcut::Paste => "Ctrl+V",
            Shortcut::SelectAll => "Ctrl+A",
            Shortcut::Find => "Ctrl+F",
            Shortcut::Replace => "Ctrl+Shift+H",
            Shortcut::GoToLine => "Ctrl+G",
            Shortcut::ToggleWrap => "Ctrl+W",
            Shortcut::ToggleHidden => "Ctrl+H",
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
            Shortcut::ToggleSidebar => "files",
            Shortcut::Save => "save",
            Shortcut::Reload => "reload",
            Shortcut::Copy => "copy",

            Shortcut::Cut => "cut",
            Shortcut::Paste => "paste",
            Shortcut::SelectAll => "all",
            Shortcut::Find => "search",
            Shortcut::Replace => "replace",
            Shortcut::GoToLine => "line",
            Shortcut::ToggleWrap => "wrap",
            Shortcut::ToggleHidden => "hidden",
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
            Shortcut::ToggleSidebar => "show or hide the file sidebar",
            Shortcut::Save => "save the current file (asks for a name if untitled)",
            Shortcut::Reload => {
                "reload the current file from disk (refuses unsaved changes; sidebar refreshes too)"
            }
            Shortcut::Copy => "copy the selection to the clipboard",

            Shortcut::Cut => "cut the selection to the clipboard",
            Shortcut::Paste => "paste from the clipboard",
            Shortcut::SelectAll => "select the whole buffer",
            Shortcut::Find => "search — type to filter, Enter/Shift+Enter next/prev, Esc closes",
            Shortcut::Replace => {
                "find and replace — type find, Tab/Enter replacement, Enter one, Shift+Enter all, Esc closes"
            }
            Shortcut::GoToLine => "go to a line number (1-based)",
            Shortcut::ToggleWrap => "toggle word wrapping of long lines",
            Shortcut::ToggleHidden => "show or hide dotfiles in the sidebar (never hides ..)",
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
            Shortcut::ToggleSidebar => PALETTE.info,
            Shortcut::Save => PALETTE.success,
            Shortcut::Reload => PALETTE.accent,
            Shortcut::Find => PALETTE.warning,
            Shortcut::Replace => PALETTE.warning,
            Shortcut::GoToLine => PALETTE.warning,
            Shortcut::ToggleWrap => PALETTE.info,
            Shortcut::ToggleHidden => PALETTE.info,
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
    /// Text buffer (and wrap/cursor/path) stashed while an image preview is
    /// open, restored when the preview is closed with Esc.
    previous_buffer: Option<Buffer>,
    /// Truecolor keeps Catppuccin RGB; ANSI 16 is used only when the
    /// terminal did not advertise truecolor. Tests default to truecolor.
    color_support: ColorSupport,
    /// Transient status message with expiry.
    message: Option<(String, Instant)>,
    /// Active "save as" input text, when the buffer has no file name.
    save_as_input: Option<String>,
    /// Active "go to line" input text, `None` while the prompt is closed.
    goto_line_input: Option<String>,
    /// Active incremental search (Ctrl+F) or find-and-replace
    /// (Ctrl+Shift+H), `None` while not searching.
    search: Option<Search>,
    /// Whether the file-tree sidebar is shown. Hidden state keeps the
    /// sidebar's directory and selection so toggling it back restores them.
    sidebar_visible: bool,
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
        // Existing image files start a preview (decode failures are errors,
        // like read failures for text files). A path that does not exist yet
        // opens an empty buffer bound to that path; the file is created on
        // the first save.
        let (buffer, image) = match &file {
            Some(path) if path.exists() && image_view::is_image_path(path) => (
                Buffer::empty(),
                Some(ImagePreview::open_with_cell_size(
                    path.clone(),
                    &picker,
                    logical_cell_size,
                )?),
            ),
            Some(path) if path.exists() => (Buffer::from_path(path.clone())?, None),
            Some(path) => (Buffer::empty_at(path.clone()), None),
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
            previous_buffer: None,
            color_support: ColorSupport::TrueColor,
            message: None,
            save_as_input: None,
            goto_line_input: None,
            search: None,
            sidebar_visible: true,
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

    /// Save-as and go-to-line steal typing from the editor.
    fn text_prompt_active(&self) -> bool {
        self.save_as_input.is_some() || self.goto_line_input.is_some()
    }

    /// Select the color path. Tests leave this at truecolor so Catppuccin
    /// RGB assertions keep matching Ghostty; production sets it from the
    /// advertised terminal capability.
    pub(crate) fn set_color_support(&mut self, support: ColorSupport) {
        self.color_support = support;
    }

    fn pal(&self) -> ratatui_themes::ThemePalette {
        theme::ui_palette(self.color_support, PALETTE)
    }

    fn paint_style(&self, style: Style) -> Style {
        theme::adapt_style(self.color_support, style)
    }

    fn pane_block(&self, title: String, title_fg: Color, focused: bool) -> Block<'static> {
        let (title_style, border_style) = if focused {
            theme::focused_pane_styles(self.color_support, PALETTE, title_fg)
        } else {
            theme::unfocused_pane_styles(self.color_support, PALETTE, title_fg)
        };
        Block::bordered()
            .title(Span::styled(title, title_style))
            .title_style(title_style)
            .border_style(border_style)
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
        self.buffer.apply_parinfer();
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
        self.previous_buffer = None;
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
        self.buffer.sync_parinfer_prev();
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

    /// Re-read the currently open file — the text buffer or the image
    /// preview — from disk, and refresh the sidebar listing so files that
    /// were added, removed or renamed show up. The reload is refused while
    /// the buffer has unsaved changes (they would be lost), but the sidebar
    /// still refreshes in every case.
    fn reload_from_disk(&mut self) {
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
                    buffer.sync_parinfer_prev();
                    self.buffer = buffer;
                    self.highlighter.set_path(Some(&path));
                    self.quit_armed = false;
                    // search matches were computed against the old text
                    if self.search.is_some() {
                        self.recompute_search();
                    }
                    let (w, h) = self.editor_text;
                    self.buffer.ensure_visible(h as usize, w as usize);
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

    // ---- search ------------------------------------------------------------

    /// Open the search bar. Ctrl+F while it is already open jumps to the
    /// next match (like most editors).
    fn open_search(&mut self) {
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
    fn open_replace(&mut self) {
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
    fn append_to_search(&mut self, text: &str) {
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
    fn replace_current_match(&mut self) {
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
    fn replace_all_matches(&mut self) {
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
    fn open_goto_line(&mut self) {
        if self.save_as_input.is_some() {
            return;
        }
        self.search = None;
        if self.goto_line_input.is_none() {
            self.goto_line_input = Some(String::new());
        }
    }

    fn confirm_goto_line(&mut self) {
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
                self.focus = Focus::Editor;
                self.ensure_cursor_visible();
            }
            Ok(_) => self.set_message(format!("line {trimmed} is out of range (1–{last})")),
            Err(_) => self.set_message("invalid line number"),
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
    fn stash_buffer_for_preview(&mut self) {
        if self.image.is_none() {
            self.previous_buffer = Some(std::mem::replace(&mut self.buffer, Buffer::empty()));
        } else {
            self.buffer = Buffer::empty();
        }
    }

    /// Close the image preview and restore the previously open buffer
    /// (including wrap). With no stashed buffer this lands on empty untitled,
    /// which is the startup-preview case.
    fn close_image_preview(&mut self) {
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
    fn toggle_hidden_files(&mut self) {
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
    fn toggle_sidebar(&mut self) {
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

    // ---- drawing -----------------------------------------------------------

    pub fn draw(&mut self, frame: &mut Frame) {
        // Paint the terminal with the theme's base color first. Individual
        // widgets and syntax spans then layer their semantic foregrounds and
        // backgrounds over it.
        frame.render_widget(
            Block::default().style(Style::default().bg(self.pal().bg)),
            frame.area(),
        );

        let pills = self.shortcut_pills();
        let [top_area, main, status_area] = Layout::vertical([
            Constraint::Length(shortcut_bar_height(&pills, frame.area().width)),
            Constraint::Min(0),
            Constraint::Length(STATUS_HEIGHT),
        ])
        .areas(frame.area());
        let (side_area, edit_area) = if self.sidebar_visible {
            let [side, edit] =
                Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)])
                    .areas(main);
            (side, edit)
        } else {
            (Rect::new(0, 0, 0, 0), main)
        };
        self.topbar_area = top_area;
        self.sidebar_area = side_area;
        self.editor_area = edit_area;

        self.draw_topbar(frame, top_area, &pills);
        if self.sidebar_visible {
            self.draw_sidebar(frame, side_area);
        }
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
                Shortcut::Reload,
                Shortcut::SwitchFocus,
                Shortcut::Quit,
            ]
        } else {
            vec![
                Shortcut::SwitchFocus,
                Shortcut::ToggleSidebar,
                Shortcut::ToggleHidden,
                Shortcut::ToggleWrap,
                Shortcut::NewFile,
                Shortcut::Reload,
                Shortcut::Undo,
                Shortcut::Redo,
                Shortcut::Copy,
                Shortcut::Cut,
                Shortcut::Paste,
                Shortcut::SelectAll,
                Shortcut::Find,
                Shortcut::Replace,
                Shortcut::GoToLine,
                Shortcut::Save,
                Shortcut::Quit,
            ]
        }
    }

    /// The clickable shortcut buttons ("Ctrl+O switch", …) across the top,
    /// separated by " | " and indented one cell from the window edge.
    /// Buttons wrap onto extra rows when they don't fit. Quit is pinned
    /// so it is never dropped; any other overflow is marked with an
    /// ellipsis. Button rectangles from the last draw are kept for mouse
    /// hit-testing.
    fn draw_topbar(&mut self, frame: &mut Frame, area: Rect, pills: &[Shortcut]) {
        self.topbar_buttons.clear();
        let hovered = self.hovered;
        let layout = wrap_topbar_pills(pills, area.width);
        let n_rows = layout.pills.last().map(|p| p.row + 1).unwrap_or(1).max(1);
        let last_body_row = layout
            .pills
            .iter()
            .rev()
            .find(|p| p.action != Shortcut::Quit)
            .map(|p| p.row);

        for row in 0..n_rows {
            let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
            let mut first = true;
            for pill in layout.pills.iter().filter(|p| p.row == row) {
                if !first {
                    spans.push(Span::styled(
                        TOPBAR_SEPARATOR,
                        Style::default().fg(self.pal().muted),
                    ));
                }
                first = false;
                self.topbar_buttons.push((
                    pill.action,
                    Rect::new(area.x + pill.x, area.y + row, pill.width, 1),
                ));
                spans.extend(pill_spans(
                    pill.action,
                    hovered == Some(pill.action),
                    self.color_support,
                ));
            }
            if layout.truncated
                && last_body_row == Some(row)
                && !layout
                    .pills
                    .iter()
                    .any(|p| p.row == row && p.action == Shortcut::Quit)
            {
                spans.push(Span::styled("…", Style::default().fg(self.pal().muted)));
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect::new(area.x, area.y + row, area.width, 1),
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
                Kind::Parent => Style::default().fg(self.pal().muted),
                Kind::Dir => Style::default().fg(self.pal().info),
                Kind::File => Style::default().fg(self.pal().fg),
            };
            if entry.is_hidden() {
                style = style.add_modifier(Modifier::DIM);
            }
            let selected = i == self.sidebar.selected;
            if selected {
                style = style.bg(self.pal().selection);
            }
            let marker = if selected { "▶ " } else { "  " };
            rows.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(self.pal().warning)),
                Span::styled(display, style),
            ]));
        }
        if rows.is_empty() {
            rows.push(Line::from(Span::styled(
                "(empty)",
                Style::default().fg(self.pal().muted),
            )));
        }

        let title = truncate(&self.sidebar.dir.display().to_string(), area.width as usize);
        let block = self.pane_block(title, PALETTE.info, self.focus == Focus::Sidebar);
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

        let block = self.pane_block(title, PALETTE.success, self.focus == Focus::Editor);

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
        frame.render_widget(block, area);

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
        let muted = self.pal().muted;
        let warning = self.pal().warning;

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
                    && self.goto_line_input.is_none()
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
                            Style::default().fg(muted),
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
                    let chunk_cols = line
                        .chars()
                        .skip(cstart)
                        .take(cend.saturating_sub(cstart))
                        .map(char_width)
                        .sum();
                    spans.extend(clip_ops(
                        line,
                        ops,
                        cstart,
                        chunk_cols,
                        sel,
                        &search_matches,
                        self.color_support,
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
                    Style::default().fg(self.pal().muted),
                )));
            }

            let paragraph = Paragraph::new(rows);
            frame.render_widget(paragraph, inner);

            if self.focus == Focus::Editor && !self.text_prompt_active() {
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
                            self.paint_style(caret_style.unwrap_or_default().bg(warning)),
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
                Style::default().fg(self.pal().muted),
            );
            let ops = self.highlighter.highlight_line(&self.buffer.lines, y);
            let line = &self.buffer.lines[y];
            if self.focus == Focus::Editor
                && self.save_as_input.is_none()
                && self.goto_line_input.is_none()
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
                self.color_support,
            ));
            rows.push(Line::from(spans));
        }
        if rows.is_empty() {
            rows.push(Line::from(Span::styled(
                "(empty)",
                Style::default().fg(self.pal().muted),
            )));
        }

        let paragraph = Paragraph::new(rows);
        frame.render_widget(paragraph, inner);

        if self.focus == Focus::Editor && !self.text_prompt_active() {
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
                        self.paint_style(caret_style.unwrap_or_default().bg(warning)),
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
            Style::default().fg(self.pal().accent)
        } else {
            Style::default().fg(self.pal().muted)
        };
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .track_style(Style::default().fg(self.pal().muted))
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
                        .fg(self.pal().warning)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(input.clone(), Style::default().fg(self.pal().fg)),
            ]))
            .style(Style::default().bg(self.pal().bg));
            frame.render_widget(paragraph, area);
            frame.set_cursor_position(Position::new(area.x + prompt_w + input_w, area.y));
            return;
        }

        // "go to line" prompt replaces the status bar content
        if let Some(input) = &self.goto_line_input {
            let prompt = "go to line: ";
            let prompt_w = prompt.width() as u16;
            let input_w = input.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>() as u16;
            let paragraph = Paragraph::new(Line::from(vec![
                Span::styled(
                    prompt,
                    Style::default()
                        .fg(self.pal().warning)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(input.clone(), Style::default().fg(self.pal().fg)),
            ]))
            .style(Style::default().bg(self.pal().bg));
            frame.render_widget(paragraph, area);
            frame.set_cursor_position(Position::new(area.x + prompt_w + input_w, area.y));
            return;
        }

        // the search / replace prompt replaces the status bar content while active
        if let Some(search) = &self.search {
            let editing_replacement =
                search.is_replace() && search.field == SearchField::Replacement;
            let (prompt, value) = if editing_replacement {
                ("replace: ", search.replacement.clone().unwrap_or_default())
            } else if search.is_replace() {
                ("find: ", search.query.clone())
            } else {
                ("search: ", search.query.clone())
            };
            let prompt_w = prompt.width() as u16;
            let input_w = value.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>() as u16;
            let base = Style::default().bg(self.pal().bg);
            let counter: Vec<Span> = if search.query.is_empty() {
                Vec::new()
            } else if search.match_count() == 0 {
                vec![Span::styled(
                    "no matches",
                    Style::default()
                        .fg(self.pal().warning)
                        .add_modifier(Modifier::BOLD),
                )]
            } else {
                vec![Span::styled(
                    format!("{}/{}", search.current_index() + 1, search.match_count()),
                    Style::default().fg(self.pal().info),
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
                            .fg(self.pal().warning)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(value, Style::default().fg(self.pal().fg)),
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
        let base = Style::default().bg(self.pal().bg).fg(self.pal().fg);

        // left: focus + file + modified state, or a transient message
        let (left_spans, left_style): (Vec<Span>, Style) =
            if let Some((msg, expiry)) = &self.message {
                if *expiry > Instant::now() {
                    // keep the modified indicator visible even while a message
                    // is showing
                    let mut spans = vec![Span::styled(
                        msg.clone(),
                        Style::default().fg(self.pal().warning),
                    )];
                    if self.buffer.dirty {
                        spans.push(Span::styled(
                            " ● modified",
                            Style::default()
                                .fg(self.pal().warning)
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
                            .fg(self.pal().secondary)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!(" {path} ")),
                    Span::styled(dims, Style::default().fg(self.pal().muted)),
                    Span::styled(view, Style::default().fg(self.pal().info)),
                ],
                Style::default(),
            );
        }
        let (tag, tag_color) = match self.focus {
            Focus::Sidebar => ("SIDEBAR", self.pal().info),
            Focus::Editor => ("EDITOR", self.pal().success),
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
                Span::styled(syntax, Style::default().fg(self.pal().muted)),
                Span::styled(wrap, Style::default().fg(self.pal().info)),
                Span::styled(
                    dirty,
                    if self.buffer.dirty {
                        Style::default()
                            .fg(self.pal().warning)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(self.pal().success)
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
fn pill_spans(action: Shortcut, hovered: bool, color_support: ColorSupport) -> Vec<Span<'static>> {
    let pal = theme::ui_palette(color_support, PALETTE);
    let hover_bg = theme::adapt_color(color_support, TOPBAR_PILL_BG_HOVER);
    let mut key_style = Style::default()
        .fg(theme::adapt_color(color_support, action.key_color()))
        .add_modifier(Modifier::BOLD);
    let mut label_style = Style::default().fg(if hovered { pal.fg } else { pal.muted });
    if hovered {
        key_style = key_style.bg(hover_bg);
        label_style = label_style.bg(hover_bg);
    }
    vec![
        Span::styled(format!("{} ", action.key_label()), key_style),
        Span::styled(action.action_label(), label_style),
    ]
}

/// How many rows the top bar needs at `width`. Must stay in sync with
/// [`App::draw_topbar`] via [`wrap_topbar_pills`].
fn shortcut_bar_height(pills: &[Shortcut], width: u16) -> u16 {
    wrap_topbar_pills(pills, width)
        .pills
        .last()
        .map(|p| p.row + 1)
        .unwrap_or(1)
        .max(1)
}

/// One shortcut button after wrapping.
#[derive(Clone, Copy, Debug)]
struct PlacedPill {
    action: Shortcut,
    x: u16,
    row: u16,
    width: u16,
}

struct TopbarLayout {
    pills: Vec<PlacedPill>,
    truncated: bool,
}

/// Wrap shortcut buttons into rows. A trailing Quit is pinned: it is always
/// placed, wrapping onto a new row if needed, and the last allowed body row
/// reserves space for it so it cannot be the button eaten by the ellipsis.
fn wrap_topbar_pills(pills: &[Shortcut], width: u16) -> TopbarLayout {
    let (body, quit) = match pills.split_last() {
        Some((&Shortcut::Quit, rest)) => (rest, Some(Shortcut::Quit)),
        _ => (pills, None),
    };
    let quit_w = quit.map(pill_width).unwrap_or(0);

    let mut out: Vec<PlacedPill> = Vec::new();
    let mut row: u16 = 0;
    let mut x = TOPBAR_INDENT;
    let mut started_row = false;
    let mut truncated = false;
    let mut body_index = 0;

    while body_index < body.len() {
        let action = body[body_index];
        let w = pill_width(action);
        let last_row = row + 1 >= TOPBAR_MAX_ROWS;
        let reserve = if quit.is_some() && last_row {
            if started_row {
                TOPBAR_SEPARATOR_WIDTH + quit_w
            } else {
                quit_w
            }
        } else {
            0
        };
        let limit = width.saturating_sub(reserve);

        if started_row && x + w > limit {
            if last_row {
                truncated = true;
                break;
            }
            row += 1;
            x = TOPBAR_INDENT;
            started_row = false;
            continue;
        }
        if x + w > width {
            truncated = true;
            body_index += 1;
            continue;
        }

        out.push(PlacedPill {
            action,
            x,
            row,
            width: w,
        });
        x = x.saturating_add(w).saturating_add(TOPBAR_SEPARATOR_WIDTH);
        started_row = true;
        body_index += 1;
    }
    if body_index < body.len() {
        truncated = true;
    }

    if let Some(action) = quit {
        let w = pill_width(action);
        if started_row && x + w > width {
            row += 1;
            x = TOPBAR_INDENT;
        }
        out.push(PlacedPill {
            action,
            x,
            row,
            width: w,
        });
    }

    TopbarLayout {
        pills: out,
        truncated,
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

/// Clip styled ranges from the highlighter to the visible slice starting at
/// `start_char` and occupying at most `max_columns` display columns (tabs
/// expand to the editor tab width). Interpreting this as a char count let
/// unwrapped long lines (and tab-expanded text) paint over the editor's
/// right border.
///
/// `None` styles render as plain text; spans overlapping `sel` (a byte range
/// on this line) get the theme's selection background, and spans inside a
/// search match (`matches`, char ranges with a "current match" flag) get the
/// match background. On ANSI-16 terminals, RGB styles are mapped; the
/// truecolor path leaves them unchanged.
fn clip_ops<'a>(
    line: &'a str,
    ops: &[(Option<Style>, Range<usize>)],
    start_char: usize,
    max_columns: usize,
    sel: Option<(usize, usize)>,
    matches: &[(usize, usize, bool)],
    color_support: ColorSupport,
) -> Vec<Span<'a>> {
    let pal = theme::ui_palette(color_support, PALETTE);
    let search_current = theme::adapt_color(color_support, SEARCH_CURRENT_BG);
    let search_other = theme::adapt_color(color_support, SEARCH_OTHER_BG);
    let mut cols = 0;
    let mut end_char = start_char;
    for c in line.chars().skip(start_char) {
        let cw = char_width(c);
        if cols + cw > max_columns {
            break;
        }
        cols += cw;
        end_char += 1;
    }
    let start_byte = char_index_to_byte(line, start_char);
    let end_byte = char_index_to_byte(line, end_char);
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
            let mut style = theme::adapt_style(color_support, style.unwrap_or_default());
            if let Some(&(_, _, current)) =
                matches.iter().find(|&&(ma, mb, _)| ca >= ma && cb <= mb)
            {
                let bg = if current {
                    search_current
                } else {
                    search_other
                };
                style = style.bg(bg);
            }
            if sel.is_some_and(|(sa, sb)| ca >= sa && cb <= sb) {
                if style.fg.is_none() {
                    style = style.fg(pal.fg);
                }
                style = style.bg(pal.selection);
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
    fn missing_file_arg_opens_an_empty_buffer_bound_to_that_path() {
        let dir = scratch("missingarg");
        let file = dir.join("somefile.md");
        assert!(!file.exists());
        let mut app = new_app(dir, Some(file.clone())).unwrap();
        assert_eq!(app.focus, Focus::Editor);
        assert_eq!(app.buffer.lines, vec![""]);
        assert_eq!(app.buffer.path.as_ref(), Some(&file));
        assert!(!app.buffer.dirty);
        assert!(app.image.is_none());
        assert!(!file.exists(), "the file must not be created until save");

        let rows = render(&mut app);
        assert!(row_contains(&rows, "somefile.md"));
        assert!(!row_contains(&rows, "untitled"));

        // Ctrl+S writes the bound path without a save-as prompt
        app.handle_key(char_key('x'));
        assert!(app.buffer.dirty);
        app.handle_key(ctrl('s'));
        assert!(app.save_as_input.is_none());
        assert!(!app.buffer.dirty);
        assert_eq!(fs::read_to_string(&file).unwrap(), "x");
    }

    #[test]
    fn missing_image_extension_arg_opens_a_text_buffer_not_a_preview() {
        let dir = scratch("missingimg");
        let file = dir.join("new.png");
        let app = new_app(dir, Some(file.clone())).unwrap();
        assert!(app.image.is_none());
        assert_eq!(app.buffer.path.as_ref(), Some(&file));
        assert_eq!(app.buffer.lines, vec![""]);
        assert!(!file.exists());
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
        let file = dir.join("notes.txt");
        // Self-contained fixture: a few short lines, then a wrapping line
        // with the search term on a continuation visual row. The previous
        // version opened README.md and hardcoded a cursor line, which
        // drifted whenever the docs changed.
        let long_line = "Installs as the `rat` command (the binary name is set explicitly in Cargo.toml, separate from the package name). Requires a Rust toolchain with Edition 2024 support. Works in any terminal that supports crossterm event and drawing APIs.";
        let contents = format!(
            "{}\n{long_line}\n",
            (0..8)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        fs::write(&file, &contents).unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();

        render_sized(&mut app, 60, 24); // establish the viewport width
        app.handle_key(ctrl('w'));
        render_sized(&mut app, 60, 24); // establish the wrapped rows

        let query = "command";
        let match_line = app
            .buffer
            .lines
            .iter()
            .position(|line| line.contains(query))
            .expect("fixture contains the search term");
        let match_start = app.buffer.lines[match_line]
            .find(query)
            .expect("search term is on that line");
        let wrap_width = app.buffer.wrap_width.max(1);
        let match_vrow =
            crate::buffer::visual_row_of(&app.buffer.lines[match_line], match_start, wrap_width);
        assert!(
            match_vrow > 0,
            "fixture must wrap so the match sits on a continuation row"
        );
        assert!(match_line > 0, "fixture needs a previous logical line");

        open_search_typed(&mut app, query);
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Up));
        // Up from a continuation row stays on the same logical line.
        assert_eq!(app.buffer.cursor.1, match_line);
        assert_eq!(
            crate::buffer::visual_row_of(
                &app.buffer.lines[match_line],
                app.buffer.cursor.0,
                wrap_width
            ),
            match_vrow - 1
        );

        // Redraw after the first move, then click a few characters into the
        // only search result as a user would.
        render_sized(&mut app, 60, 24);
        let target = (match_start + 3, match_line);
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
        assert_eq!(app.buffer.cursor.1, match_line);
        assert_eq!(
            crate::buffer::visual_row_of(
                &app.buffer.lines[match_line],
                app.buffer.cursor.0,
                wrap_width
            ),
            match_vrow - 1
        );
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.buffer.cursor.1, match_line - 1);
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

    // ---- find and replace (Ctrl+Shift+H) ----------------------------------

    #[test]
    fn ctrl_shift_h_opens_replace_and_replaces_current_then_all() {
        let dir = scratch("replace1");
        let file = dir.join("a.txt");
        fs::write(&file, "foo bar foo\nfoo\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();

        app.handle_key(ctrl_shift('h'));
        assert!(app.search.as_ref().is_some_and(|s| s.is_replace()));
        assert_eq!(
            app.search.as_ref().unwrap().field,
            crate::search::SearchField::Query
        );
        for c in "foo".chars() {
            app.handle_key(char_key(c));
        }
        assert_eq!(app.search.as_ref().unwrap().match_count(), 3);

        // Enter on the find field moves to the replacement
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            app.search.as_ref().unwrap().field,
            crate::search::SearchField::Replacement
        );
        for c in "qux".chars() {
            app.handle_key(char_key(c));
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.buffer.lines, vec!["qux bar foo", "foo", ""]);
        assert_eq!(app.search.as_ref().unwrap().match_count(), 2);

        // Shift+Enter replaces the rest
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.buffer.lines, vec!["qux bar qux", "qux", ""]);
        assert!(
            app.message
                .as_ref()
                .is_some_and(|(msg, _)| msg.contains("replaced 2"))
        );

        app.handle_key(key(KeyCode::Esc));
        assert!(app.search.is_none());
    }

    #[test]
    fn replace_reuses_an_open_search_query() {
        let dir = scratch("replace2");
        let file = dir.join("a.txt");
        fs::write(&file, "hello hello\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        open_search_typed(&mut app, "hello");
        app.handle_key(ctrl_shift('h'));
        let search = app.search.as_ref().unwrap();
        assert!(search.is_replace());
        assert_eq!(search.query, "hello");
        assert_eq!(search.field, crate::search::SearchField::Replacement);

        for c in "hi".chars() {
            app.handle_key(char_key(c));
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.buffer.lines[0], "hi hello");
    }

    #[test]
    fn replace_with_no_matches_is_a_status_message() {
        let dir = scratch("replace3");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        app.handle_key(ctrl_shift('h'));
        for c in "zzz".chars() {
            app.handle_key(char_key(c));
        }
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(char_key('x'));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.buffer.lines, vec!["alpha", ""]);
        assert!(
            app.message
                .as_ref()
                .is_some_and(|(msg, _)| msg.contains("no matches"))
        );
    }

    #[test]
    fn replace_undoes_one_replacement_and_replace_all_as_one_step() {
        let dir = scratch("replace4");
        let file = dir.join("a.txt");
        fs::write(&file, "aa aa aa").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        app.handle_key(ctrl_shift('h'));
        app.handle_key(char_key('a'));
        app.handle_key(char_key('a'));
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(char_key('b'));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.buffer.lines, vec!["b aa aa"]);
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(ctrl('z'));
        assert_eq!(app.buffer.lines, vec!["aa aa aa"]);

        app.handle_key(ctrl_shift('h'));
        app.handle_key(char_key('a'));
        app.handle_key(char_key('a'));
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(char_key('b'));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.buffer.lines, vec!["b b b"]);
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(ctrl('z'));
        assert_eq!(app.buffer.lines, vec!["aa aa aa"]);
    }

    #[test]
    fn ctrl_h_during_replace_still_toggles_dotfiles() {
        let dir = scratch("replace5");
        fs::write(dir.join(".env"), "SECRET=1").unwrap();
        fs::write(dir.join("a.txt"), "foo").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        app.handle_key(ctrl_shift('h'));
        assert!(app.sidebar.hide_dotfiles);
        app.handle_key(ctrl('h'));
        assert!(!app.sidebar.hide_dotfiles);
        assert!(app.search.as_ref().is_some_and(|s| s.is_replace()));
    }

    #[test]
    fn cmd_shift_h_opens_replace_like_ctrl_shift_h() {
        let dir = scratch("replace6");
        fs::write(dir.join("a.txt"), "foo").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        app.handle_key(cmd_shift('h'));
        assert!(app.search.as_ref().is_some_and(|s| s.is_replace()));
    }

    #[test]
    fn ghostty_ctrl_shift_h_is_replace_not_hide_dotfiles() {
        // Ghostty with the kitty protocol and REPORT_ALTERNATE_KEYS folds
        // Shift into the character and drops the SHIFT modifier, so
        // Ctrl+Shift+H arrives as 'H'+CONTROL — the same shape as
        // Ctrl+Shift+Z. That must open replace, not toggle hide-dotfiles.
        let events = [
            KeyEvent::new(KeyCode::Char('H'), KeyModifiers::CONTROL),
            KeyEvent::new(
                KeyCode::Char('H'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            KeyEvent::new(
                KeyCode::Char('h'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SUPER),
            KeyEvent::new(
                KeyCode::Char('h'),
                KeyModifiers::SUPER | KeyModifiers::SHIFT,
            ),
        ];
        for (i, event) in events.into_iter().enumerate() {
            let dir = scratch(&format!("ghostty-csh-{i}"));
            fs::write(dir.join(".env"), "SECRET=1").unwrap();
            fs::write(dir.join("a.txt"), "hello").unwrap();
            let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
            assert!(app.sidebar.hide_dotfiles);
            app.handle_key(event);
            assert!(
                app.search.as_ref().is_some_and(|s| s.is_replace()),
                "event {event:?} must open replace"
            );
            assert!(
                app.sidebar.hide_dotfiles,
                "event {event:?} must not toggle hide-dotfiles"
            );
        }
    }

    #[test]
    fn unshifted_ctrl_h_still_toggles_dotfiles_not_replace() {
        let dir = scratch("ctrl-h-not-replace");
        fs::write(dir.join(".env"), "SECRET=1").unwrap();
        fs::write(dir.join("a.txt"), "hello").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        app.handle_key(ctrl('h'));
        assert!(!app.sidebar.hide_dotfiles);
        assert!(app.search.is_none());
        app.handle_key(cmd('h'));
        assert!(app.sidebar.hide_dotfiles);
        assert!(app.search.is_none());
    }

    // ---- go to line (Ctrl+G) ----------------------------------------------

    #[test]
    fn ctrl_g_jumps_to_a_1_based_line_and_keeps_it_visible() {
        let dir = scratch("goto1");
        let file = dir.join("a.txt");
        let contents: String = (1..=40).map(|n| format!("line {n}\n")).collect();
        fs::write(&file, &contents).unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        render_sized(&mut app, 80, 12);

        app.handle_key(ctrl('g'));
        assert_eq!(app.goto_line_input.as_deref(), Some(""));
        let rows = render(&mut app);
        assert!(row_contains(&rows, "go to line:"));

        app.handle_key(char_key('3'));
        app.handle_key(char_key('5'));
        app.handle_key(key(KeyCode::Enter));
        assert!(app.goto_line_input.is_none());
        assert_eq!(app.buffer.cursor, (0, 34));
        assert_eq!(app.focus, Focus::Editor);
        // the jumped-to line must be inside the viewport
        let (_w, h) = app.editor_text;
        assert!(app.buffer.cursor.1 >= app.buffer.scroll.1);
        assert!(app.buffer.cursor.1 < app.buffer.scroll.1 + h as usize);
    }

    #[test]
    fn ctrl_g_invalid_input_is_a_status_message() {
        let dir = scratch("goto2");
        let file = dir.join("a.txt");
        fs::write(&file, "one\ntwo\nthree\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        let cursor = app.buffer.cursor;

        app.handle_key(ctrl('g'));
        for c in "nope".chars() {
            app.handle_key(char_key(c));
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.buffer.cursor, cursor);
        assert!(
            app.message
                .as_ref()
                .is_some_and(|(msg, _)| msg.contains("invalid line number"))
        );

        app.handle_key(ctrl('g'));
        app.handle_key(char_key('0'));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.buffer.cursor, cursor);
        assert!(
            app.message
                .as_ref()
                .is_some_and(|(msg, _)| msg.contains("out of range") || msg.contains("invalid"))
        );

        app.handle_key(ctrl('g'));
        for c in "99".chars() {
            app.handle_key(char_key(c));
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.buffer.cursor, cursor);
        assert!(
            app.message
                .as_ref()
                .is_some_and(|(msg, _)| msg.contains("out of range"))
        );

        app.handle_key(ctrl('g'));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.goto_line_input.is_none());
    }

    // ---- toggle sidebar (Ctrl+B) ------------------------------------------

    #[test]
    fn ctrl_b_hides_the_sidebar_and_preserves_selection() {
        let dir = scratch("sidebar-toggle1");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        fs::write(dir.join("b.txt"), "beta").unwrap();
        let mut app = new_app(dir.clone(), None).unwrap();
        app.sidebar.select_name("b.txt");
        let selected = app.sidebar.selected;
        let listed = app.sidebar.dir.clone();

        app.handle_key(ctrl('b'));
        assert!(!app.sidebar_visible);
        assert_eq!(app.focus, Focus::Editor); // was sidebar; don't trap
        assert_eq!(app.sidebar.selected, selected);
        assert_eq!(app.sidebar.dir, listed);

        render_buffer(&mut app);
        let rows = render(&mut app);
        assert!(!row_contains(&rows, "b.txt"));
        assert!(app.editor_area.x < 28);

        app.handle_key(ctrl('b'));
        assert!(app.sidebar_visible);
        assert_eq!(app.sidebar.selected, selected);
        render_buffer(&mut app);
        let rows = render(&mut app);
        assert!(row_contains(&rows, "b.txt"));
    }

    #[test]
    fn ctrl_o_while_sidebar_hidden_shows_and_focuses_it() {
        let dir = scratch("sidebar-toggle2");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        assert_eq!(app.focus, Focus::Editor);
        app.handle_key(ctrl('b'));
        assert!(!app.sidebar_visible);
        assert_eq!(app.focus, Focus::Editor);

        app.handle_key(ctrl('o'));
        assert!(app.sidebar_visible);
        assert_eq!(app.focus, Focus::Sidebar);

        app.handle_key(ctrl('o'));
        assert!(app.sidebar_visible);
        assert_eq!(app.focus, Focus::Editor);
    }

    #[test]
    fn editing_topbar_has_seventeen_shortcuts_with_quit_last() {
        let dir = scratch("topbar17");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        render_buffer(&mut app);
        let actions: Vec<Shortcut> = app.topbar_buttons.iter().map(|(a, _)| *a).collect();
        assert_eq!(actions.len(), 17);
        assert_eq!(*actions.last().unwrap(), Shortcut::Quit);
        for action in [
            Shortcut::SelectAll,
            Shortcut::ToggleSidebar,
            Shortcut::GoToLine,
            Shortcut::Replace,
            Shortcut::ToggleHidden,
        ] {
            assert!(actions.contains(&action), "missing {action:?}");
        }
        let rows = render(&mut app);
        assert!(row_contains(&rows, "Ctrl+A all"));
        assert!(row_contains(&rows, "Ctrl+B files"));
        assert!(row_contains(&rows, "Ctrl+G line"));
        assert!(row_contains(&rows, "Ctrl+Shift+H replace"));
        assert!(row_contains(&rows, "Ctrl+Q quit"));
        // 150 columns is enough for the full 17-button set on two rows
        assert_eq!(app.topbar_buttons.len(), 17);
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
            ColorSupport::TrueColor,
        );
        let parts: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(parts, vec!["hello", " world"]);
        // match keeps the syntax fg and gains the match background
        assert_eq!(spans[0].style.bg, Some(SEARCH_OTHER_BG));
        assert_eq!(spans[0].style.fg, Some(Color::Blue));
        assert_eq!(spans[1].style.bg, None);
    }

    #[test]
    fn clip_ops_clips_to_display_columns_not_char_count() {
        let line = "X".repeat(40);
        let spans = clip_ops(
            &line,
            &[(None, 0..line.len())],
            0,
            8,
            None,
            &[],
            ColorSupport::TrueColor,
        );
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "XXXXXXXX");

        let line = "\tABCD";
        let spans = clip_ops(
            line,
            &[(None, 0..line.len())],
            0,
            6,
            None,
            &[],
            ColorSupport::TrueColor,
        );
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "    AB");
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

    // ---- reload (Ctrl+R) ----------------------------------------------------

    #[test]
    fn ctrl_r_reloads_the_open_file_from_disk() {
        let dir = scratch("reload1");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha\n").unwrap();
        let mut app = new_app(dir.clone(), Some(file.clone())).unwrap();
        assert_eq!(app.buffer.lines, vec!["alpha", ""]);
        app.buffer.cursor = (2, 0);
        app.handle_key(ctrl('w')); // wrapping survives the reload
        assert!(app.buffer.wrap);

        // the file changes on disk (e.g. by another program)
        fs::write(&file, "beta\ngamma\n").unwrap();
        app.handle_key(ctrl('r'));
        assert_eq!(app.buffer.lines, vec!["beta", "gamma", ""]);
        assert!(!app.buffer.dirty);
        assert_eq!(app.buffer.path, Some(file));
        assert!(app.buffer.wrap, "wrapping survives a reload");
        assert_eq!(app.buffer.cursor, (2, 0));
        assert!(app.message.is_some());
    }

    #[test]
    fn cmd_r_reloads_like_ctrl_r() {
        let dir = scratch("reloadcmd");
        let file = dir.join("a.txt");
        fs::write(&file, "one").unwrap();
        let mut app = new_app(dir.clone(), Some(file)).unwrap();
        fs::write(dir.join("a.txt"), "two").unwrap();
        app.handle_key(cmd('r'));
        assert_eq!(app.buffer.lines, vec!["two"]);
    }

    #[test]
    fn ctrl_r_refuses_unsaved_changes_but_still_refreshes_the_sidebar() {
        let dir = scratch("reload2");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha\n").unwrap();
        let mut app = new_app(dir.clone(), Some(file.clone())).unwrap();
        app.handle_key(char_key('X'));
        assert!(app.buffer.dirty);

        fs::write(&file, "changed on disk\n").unwrap();
        fs::write(dir.join("added.txt"), "new\n").unwrap();
        app.handle_key(ctrl('r'));
        // the in-memory edit survives; the disk change is not loaded
        assert_eq!(app.buffer.lines, vec!["Xalpha", ""]);
        assert_eq!(
            app.buffer.path.as_ref().unwrap().file_name().unwrap(),
            "a.txt"
        );
        // the sidebar still reflects the external changes
        assert!(app.sidebar.entries.iter().any(|e| e.name == "added.txt"));
        assert!(app.message.is_some());
    }

    #[test]
    fn ctrl_r_without_a_file_shows_a_message() {
        let dir = scratch("reload3");
        let mut app = new_app(dir, None).unwrap();
        app.handle_key(ctrl('r'));
        assert_eq!(app.buffer.lines, vec![""]);
        assert!(app.message.is_some());
    }

    #[test]
    fn ctrl_r_reload_failure_keeps_the_buffer_and_refreshes_the_sidebar() {
        let dir = scratch("reload5");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha\n").unwrap();
        let mut app = new_app(dir.clone(), Some(file.clone())).unwrap();
        fs::remove_file(&file).unwrap();

        app.handle_key(ctrl('r'));
        // the failed reload leaves the buffer as it was...
        assert_eq!(app.buffer.lines, vec!["alpha", ""]);
        assert!(!app.buffer.dirty);
        // ...but the sidebar shows the file is gone
        assert!(!app.sidebar.entries.iter().any(|e| e.name == "a.txt"));
        assert!(app.message.is_some());
    }

    #[test]
    fn ctrl_r_refreshes_the_sidebar_after_external_changes() {
        let dir = scratch("reload6");
        fs::write(dir.join("a.txt"), "x").unwrap();
        fs::write(dir.join("gone.txt"), "x").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        assert!(app.sidebar.entries.iter().any(|e| e.name == "gone.txt"));

        fs::remove_file(dir.join("gone.txt")).unwrap();
        fs::write(dir.join("fresh.txt"), "y").unwrap();
        app.handle_key(ctrl('r'));

        let names: Vec<&str> = app
            .sidebar
            .entries
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(names.contains(&"fresh.txt"));
        assert!(!names.contains(&"gone.txt"));
        // the open file itself is unaffected
        assert_eq!(app.buffer.lines, vec!["x"]);
    }

    #[test]
    fn ctrl_r_reloads_the_image_preview() {
        let dir = scratch("reloadimg");
        let file = dir.join("pic.png");
        write_test_png(&file);
        let mut app = new_app(dir.clone(), Some(file.clone())).unwrap();
        assert_eq!(app.image.as_ref().unwrap().pixels, (4, 2));

        let mut img = image::RgbImage::new(2, 3);
        for x in 0..2 {
            for y in 0..3 {
                img.put_pixel(x, y, image::Rgb([x as u8 * 100, y as u8 * 80, 0]));
            }
        }
        img.save(&file).unwrap();
        app.handle_key(ctrl('r'));
        let preview = app.image.as_ref().unwrap();
        assert_eq!(preview.pixels, (2, 3));
        assert_eq!(preview.path, file);
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

    fn render_rows(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let w = width as usize;
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(w)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect()
    }

    fn render(app: &mut App) -> Vec<String> {
        render_rows(app, 150, 24)
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

    /// Hit rectangle for `action` from the last draw.
    fn topbar_button(app: &App, action: Shortcut) -> Rect {
        app.topbar_buttons
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("no {action:?} button in the top bar"))
    }

    /// Click the button for `action` (built by the last draw).
    fn click_button(app: &mut App, action: Shortcut) {
        let rect = topbar_button(app, action);
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
    fn clicking_select_all_button_selects_the_buffer() {
        let dir = scratch("mbtnall");
        let file = dir.join("a.txt");
        fs::write(&file, "hello\nworld").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);
        assert!(!app.buffer.has_selection());
        click_button(&mut app, Shortcut::SelectAll);
        assert_eq!(app.buffer.selected_text().as_deref(), Some("hello\nworld"));
    }

    #[test]
    fn clicking_files_button_toggles_the_sidebar() {
        let dir = scratch("mbtnfiles");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        render_buffer(&mut app);
        let shown_width = app.editor_area.width;
        assert!(app.sidebar_visible);
        assert!(app.sidebar_area.width > 0);

        click_button(&mut app, Shortcut::ToggleSidebar);
        assert!(!app.sidebar_visible);
        render_buffer(&mut app);
        assert_eq!(app.sidebar_area.width, 0);
        assert!(app.editor_area.width > shown_width);
        assert_eq!(app.focus, Focus::Editor);

        click_button(&mut app, Shortcut::ToggleSidebar);
        assert!(app.sidebar_visible);
        render_buffer(&mut app);
        assert_eq!(app.editor_area.width, shown_width);
    }

    #[test]
    fn clicking_line_button_opens_the_goto_prompt() {
        let dir = scratch("mbtnline");
        fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        render_buffer(&mut app);
        click_button(&mut app, Shortcut::GoToLine);
        assert_eq!(app.goto_line_input.as_deref(), Some(""));
        app.handle_key(char_key('3'));
        app.handle_key(key(KeyCode::Enter));
        assert!(app.goto_line_input.is_none());
        assert_eq!(app.buffer.cursor, (0, 2));
    }

    #[test]
    fn clicking_replace_button_opens_find_and_replace() {
        let dir = scratch("mbtnreplace");
        fs::write(dir.join("a.txt"), "foo bar foo\n").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        render_buffer(&mut app);
        click_button(&mut app, Shortcut::Replace);
        assert!(app.search.as_ref().is_some_and(|s| s.is_replace()));
        for c in "foo".chars() {
            app.handle_key(char_key(c));
        }
        app.handle_key(key(KeyCode::Tab));
        for c in "qux".chars() {
            app.handle_key(char_key(c));
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.buffer.lines[0], "qux bar foo");
    }

    #[test]
    fn clicking_top_bar_gap_does_nothing() {
        let dir = scratch("mbtngap");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        render_buffer(&mut app);
        assert_eq!(app.focus, Focus::Editor);

        // after the last button on its row there is empty bar; clicking it
        // must not move focus, arm quit, or do anything else
        let last = app
            .topbar_buttons
            .iter()
            .max_by_key(|(_, r)| (r.y, r.x + r.width))
            .map(|(_, r)| *r)
            .unwrap();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            last.x + last.width + 2,
            last.y,
        ));
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
        let rect = topbar_button(&app, Shortcut::Save);

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
        let hovered = topbar_button(&app, Shortcut::Save);
        let idle = app
            .topbar_buttons
            .iter()
            .find(|(a, _)| *a != Shortcut::Save)
            .map(|(_, r)| *r)
            .expect("another shortcut button to compare against");
        assert!(hovered.width > 0);
        assert!(hovered.x >= TOPBAR_INDENT);

        // the bar starts one cell in from the window edge: cell (0,0) is
        // plain margin, not part of the first button
        assert_eq!(buf.cell((0, 0)).unwrap().style().bg, Some(PALETTE.bg));

        // moving the mouse over the button highlights that button's own
        // rectangle, including when Save has wrapped onto a later row
        app.handle_mouse(mouse(MouseEventKind::Moved, hovered.x + 1, hovered.y));
        let buf = render_buffer(&mut app);
        for x in hovered.x..hovered.x + hovered.width {
            assert_eq!(
                buf.cell((x, hovered.y)).unwrap().style().bg,
                Some(TOPBAR_PILL_BG_HOVER),
                "col {x} row {}",
                hovered.y
            );
        }
        // ...while a non-hovered button blends into the base background
        assert_eq!(
            buf.cell((idle.x, idle.y)).unwrap().style().bg,
            Some(PALETTE.bg)
        );

        // moving away restores the base background
        app.handle_mouse(mouse(MouseEventKind::Moved, 140, 10));
        let buf = render_buffer(&mut app);
        assert_eq!(
            buf.cell((hovered.x + 1, hovered.y)).unwrap().style().bg,
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

        // 80 columns: the buttons flow onto extra rows but stay visible.
        // Which labels land on which row depends on pill order and width,
        // so this checks wrapping and hit-testing rather than a snapshot.
        let rows = render_rows(&mut app, 80, 24);
        assert!(
            app.topbar_buttons.iter().any(|(_, r)| r.y > 0),
            "expected wrapping at 80 columns, buttons={:?}",
            app.topbar_buttons
        );
        assert!(app.topbar_area.height >= 2);
        assert!(row_contains(&rows, "Ctrl+O switch"));
        // each hit rectangle matches the row that actually painted that label
        for (action, rect) in &app.topbar_buttons {
            let label = format!("{} {}", action.key_label(), action.action_label());
            assert!(
                rows[rect.y as usize].contains(&label),
                "{label} missing from row {}",
                rect.y
            );
        }
        // Quit is pinned, so wrapping keeps it visible instead of dropping
        // it behind the ellipsis.
        assert_eq!(app.topbar_buttons.last().unwrap().0, Shortcut::Quit);
        assert!(row_contains(&rows, "Ctrl+Q quit"));

        // clicking a button on a wrapped row still works
        let wrapped = app
            .topbar_buttons
            .iter()
            .find(|(a, r)| r.y > 0 && *a != Shortcut::Quit)
            .map(|(a, _)| *a)
            .expect("a non-quit button should wrap onto a later row");
        click_button(&mut app, wrapped);
    }

    #[test]
    fn top_bar_keeps_quit_visible_at_1280px_air_width() {
        // Ghostty on a 13" Air at 1280 logical px is ~120 columns with a
        // ~10–11px cell. Two rows used to end `Ctrl+H hidden…` and omit Quit.
        let dir = scratch("mbtnair");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();

        let rows = render_rows(&mut app, 120, 24);
        let actions: Vec<Shortcut> = app.topbar_buttons.iter().map(|(a, _)| *a).collect();
        assert_eq!(actions.len(), 17, "{actions:?}");
        assert_eq!(*actions.last().unwrap(), Shortcut::Quit);
        assert!(row_contains(&rows, "Ctrl+Q quit"));
        assert!(row_contains(&rows, "Ctrl+H hidden"));
        assert!(row_contains(&rows, "Ctrl+Shift+H replace"));
        let quit = topbar_button(&app, Shortcut::Quit);
        assert!(
            quit.y > 0,
            "Quit should wrap off the first row at 120 columns, was y={}",
            quit.y
        );
        assert!(app.topbar_area.height >= 2);
        click_button(&mut app, Shortcut::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn wrap_topbar_pills_stays_in_bounds_across_widths() {
        let dir = scratch("mbtnwrapbounds");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        let app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        let pills = app.shortcut_pills();

        for width in 40..=160 {
            let layout = wrap_topbar_pills(&pills, width);
            assert_eq!(
                layout.pills.last().map(|p| p.action),
                Some(Shortcut::Quit),
                "width {width}"
            );
            let mut prev: Option<(u16, u16, u16)> = None; // row, x, x+width
            for pill in &layout.pills {
                assert!(
                    pill.x >= TOPBAR_INDENT,
                    "width {width} {pill:?} starts before indent"
                );
                if width >= pill.width + TOPBAR_INDENT {
                    assert!(
                        pill.x + pill.width <= width,
                        "width {width} {pill:?} overflows"
                    );
                }
                if let Some((row, x, end)) = prev {
                    if row == pill.row {
                        assert!(
                            pill.x >= end,
                            "width {width} overlap: prior ends {end} at x={x}, {pill:?}"
                        );
                    }
                }
                prev = Some((pill.row, pill.x, pill.x + pill.width));
            }
        }

        let wide = wrap_topbar_pills(&pills, 150);
        assert_eq!(wide.pills.len(), pills.len());
        let narrow = wrap_topbar_pills(&pills, 80);
        assert!(
            narrow.pills.iter().any(|p| p.row > 0),
            "80 columns should wrap"
        );
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
                Shortcut::Reload,
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
        assert_eq!(actions.len(), 17);
        assert_eq!(actions[0], Shortcut::SwitchFocus);
        assert_eq!(actions[1], Shortcut::ToggleSidebar);
        assert_eq!(*actions.last().unwrap(), Shortcut::Quit);
        assert!(actions.contains(&Shortcut::SelectAll));
        assert!(actions.contains(&Shortcut::Replace));
        assert!(actions.contains(&Shortcut::GoToLine));
        assert!(actions.contains(&Shortcut::ToggleHidden));
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
            ColorSupport::TrueColor,
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
            ColorSupport::TrueColor,
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
        // the shortcut buttons live in the top bar; which row they occupy
        // depends on width and pill order
        assert!(row_contains(&rows, "Ctrl+Z undo"));
        assert!(row_contains(&rows, "Ctrl+Shift+Z redo"));
        assert!(row_contains(&rows, "Ctrl+Q quit"));
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
    fn esc_restores_previous_buffer_and_wrap_after_image_preview() {
        let dir = scratch("imgrestore");
        fs::write(dir.join("notes.txt"), "hello wrap me\n").unwrap();
        write_test_png(&dir.join("logo.png"));
        write_test_png(&dir.join("other.png"));
        let mut app = new_app(dir.clone(), Some(dir.join("notes.txt"))).unwrap();
        render_buffer(&mut app);
        app.handle_key(ctrl('w'));
        assert!(app.buffer.wrap);

        app.handle_key(ctrl('o'));
        app.sidebar.select_name("logo.png");
        app.handle_key(key(KeyCode::Enter));
        assert!(app.image.is_some());
        assert_eq!(app.buffer.lines, vec![""]);
        assert!(app.buffer.path.is_none());

        // opening another image keeps the stashed text buffer
        app.handle_key(ctrl('o'));
        app.sidebar.select_name("other.png");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.image.as_ref().unwrap().path, dir.join("other.png"));

        app.handle_key(key(KeyCode::Esc));
        assert!(app.image.is_none());
        assert_eq!(app.buffer.lines, vec!["hello wrap me", ""]);
        assert_eq!(app.buffer.path, Some(dir.join("notes.txt")));
        assert!(app.buffer.wrap);
        let rows = render(&mut app);
        assert!(row_contains(&rows, "notes.txt"));
        assert!(!row_contains(&rows, "untitled"));
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

    #[test]
    fn unwrapped_long_line_does_not_paint_over_right_border() {
        let dir = scratch("longline-border");
        let file = dir.join("wide.txt");
        fs::write(&file, format!("{}\n", "X".repeat(500))).unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        assert!(!app.buffer.wrap);
        let buf = render_buffer(&mut app);
        let border_x = app.editor_area.x + app.editor_area.width - 1;
        let y = app.editor_area.y + 1;
        let cell = buf.cell((border_x, y)).unwrap();
        assert_ne!(
            cell.symbol(),
            "X",
            "unwrapped text painted over the right border"
        );
        assert!(
            matches!(cell.symbol(), "│" | "┃" | "█" | "┤" | "┐" | "┘"),
            "expected a border or scrollbar glyph, got {:?}",
            cell.symbol()
        );

        // tabs expand to several columns; they must clip too
        let dir = scratch("longline-tabs");
        let file = dir.join("tabs.txt");
        fs::write(&file, format!("{}\n", "\t".repeat(80))).unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);
        let border_x = app.editor_area.x + app.editor_area.width - 1;
        let y = app.editor_area.y + 1;
        let cell = buf.cell((border_x, y)).unwrap();
        assert_ne!(
            cell.symbol(),
            " ",
            "tab expansion overwrote the right border"
        );
        assert!(matches!(cell.symbol(), "│" | "┃" | "█" | "┤" | "┐" | "┘"));
    }

    #[test]
    fn ctrl_h_toggles_dotfiles_in_the_sidebar() {
        let dir = scratch("apphidden");
        fs::write(dir.join(".env"), "SECRET=1").unwrap();
        fs::write(dir.join("shown.txt"), "ok").unwrap();
        let mut app = new_app(dir, None).unwrap();
        let names = |app: &App| {
            app.sidebar
                .entries
                .iter()
                .map(|e| e.name.clone())
                .collect::<Vec<_>>()
        };
        assert!(app.sidebar.hide_dotfiles);
        assert_eq!(names(&app), vec!["..".to_string(), "shown.txt".to_string()]);
        app.handle_key(ctrl('h'));
        assert!(!app.sidebar.hide_dotfiles);
        assert_eq!(
            names(&app),
            vec![
                "..".to_string(),
                ".env".to_string(),
                "shown.txt".to_string()
            ]
        );
        assert!(
            app.message
                .as_ref()
                .is_some_and(|(msg, _)| msg.contains("dotfiles shown"))
        );
        app.handle_key(ctrl('h'));
        assert!(app.sidebar.hide_dotfiles);
        assert_eq!(names(&app), vec!["..".to_string(), "shown.txt".to_string()]);
        let rows = render(&mut app);
        assert!(row_contains(&rows, "Ctrl+H hidden"));
    }

    #[test]
    fn ansi16_fallback_uses_named_colors_instead_of_rgb() {
        let dir = scratch("ansi16-ui");
        let file = dir.join("notes.txt");
        fs::write(&file, "hello\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        app.set_color_support(ColorSupport::Ansi16);
        let buf = render_buffer(&mut app);
        let border = buf
            .cell((app.editor_area.x, app.editor_area.y + 1))
            .unwrap();
        assert_eq!(border.style().fg, Some(Color::LightBlue));
        assert_eq!(buf.cell((0, 0)).unwrap().style().bg, Some(Color::Black));
        // truecolor default is unchanged
        let dir = scratch("truecolor-ui");
        let file = dir.join("notes.txt");
        fs::write(&file, "hello\n").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        let buf = render_buffer(&mut app);
        let border = buf
            .cell((app.editor_area.x, app.editor_area.y + 1))
            .unwrap();
        assert_eq!(border.style().fg, Some(FOCUS_COLOR));
        assert_eq!(buf.cell((0, 0)).unwrap().style().bg, Some(PALETTE.bg));
    }

    #[test]
    fn parinfer_runs_for_clojure_files_through_the_editor() {
        let dir = scratch("parinfer-app-clj");
        let file = dir.join("core.clj");
        fs::write(&file, "").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        app.handle_key(char_key('('));
        assert_eq!(app.buffer.lines, vec!["()"]);
        assert_eq!(app.buffer.cursor, (1, 0));
        app.handle_key(char_key('a'));
        assert_eq!(app.buffer.lines, vec!["(a)"]);
        assert_eq!(app.buffer.cursor, (2, 0));
        // one logical undo for the typing run plus the automatic parens
        app.handle_key(ctrl('z'));
        assert_eq!(app.buffer.lines, vec![""]);
        app.handle_key(KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(app.buffer.lines, vec!["(a)"]);
    }

    #[test]
    fn parinfer_leaves_non_clojure_files_alone() {
        let dir = scratch("parinfer-app-txt");
        let file = dir.join("notes.txt");
        fs::write(&file, "").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        app.handle_key(char_key('('));
        assert_eq!(app.buffer.lines, vec!["("]);
        assert_eq!(app.buffer.cursor, (1, 0));
    }

    #[test]
    fn parinfer_incomplete_string_keeps_typed_text() {
        let dir = scratch("parinfer-app-str");
        let file = dir.join("core.cljs");
        fs::write(&file, "").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        app.handle_key(char_key('"'));
        app.handle_key(char_key('h'));
        app.handle_key(char_key('i'));
        assert_eq!(app.buffer.lines, vec!["\"hi"]);
        assert_eq!(app.buffer.cursor, (3, 0));
    }

    #[test]
    fn parinfer_keyboard_navigation_does_not_rewrite_buffer() {
        let dir = scratch("parinfer-app-nav");
        let file = dir.join("core.clj");
        fs::write(&file, "(foo\nbar").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        let before = app.buffer.lines.clone();
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.buffer.cursor.1, 1);
        assert_eq!(app.buffer.lines, before);
        for code in [
            KeyCode::Right,
            KeyCode::End,
            KeyCode::Home,
            KeyCode::Up,
            KeyCode::PageDown,
            KeyCode::PageUp,
            KeyCode::Left,
        ] {
            app.handle_key(key(code));
            assert_eq!(app.buffer.lines, before, "{code:?}");
        }
        assert!(!app.buffer.dirty);
        app.handle_key(ctrl('z'));
        assert_eq!(app.buffer.lines, before, "navigation must not create undo");

        // Typing after navigation still runs Smart Mode.
        app.handle_key(key(KeyCode::End));
        app.handle_key(char_key('x'));
        assert!(app.buffer.lines.join("\n").contains('x'));
        assert!(app.buffer.dirty);
    }

    #[test]
    fn parinfer_mouse_cursor_placement_does_not_rewrite_buffer() {
        let dir = scratch("parinfer-app-mouse");
        let file = dir.join("core.clj");
        fs::write(&file, "(foo\nbar").unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        render_buffer(&mut app);
        let before = app.buffer.lines.clone();
        let inner_x = app.editor_area.x as usize + 1;
        let inner_y = app.editor_area.y as usize + 1;
        let gutter_w = app.buffer.lines.len().to_string().len() + 1;
        let text_x = (inner_x + gutter_w + 1) as u16;
        let line0_y = inner_y as u16;
        let line1_y = (inner_y + 1) as u16;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            text_x,
            line0_y,
        ));
        assert_eq!(app.focus, Focus::Editor);
        assert_eq!(app.buffer.cursor.1, 0);
        assert!(app.buffer.cursor.0 > 0, "click should land in the text");
        assert_eq!(app.buffer.lines, before);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            text_x,
            line1_y,
        ));
        assert_eq!(app.buffer.cursor.1, 1);
        assert_eq!(app.buffer.lines, before);

        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            text_x + 2,
            line1_y,
        ));
        assert_eq!(app.buffer.cursor.1, 1);
        assert_eq!(app.buffer.lines, before);
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            text_x + 2,
            line1_y,
        ));
        assert_eq!(app.buffer.lines, before);
        assert!(!app.buffer.dirty);
        app.handle_key(ctrl('z'));
        assert_eq!(
            app.buffer.lines, before,
            "mouse placement must not create undo"
        );
    }
}
