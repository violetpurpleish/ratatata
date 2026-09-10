//! Shared application state and initialization.
//!
//! Input, file/search actions, and rendering live in focused child modules;
//! their implementations share this state without exposing it outside the app.

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
#[cfg(not(target_os = "windows"))]
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
#[cfg(test)]
const SEARCH_CURRENT_BG: Color = PALETTE.warning;
/// Background of the other search matches.
#[cfg(test)]
const SEARCH_OTHER_BG: Color = PALETTE.selection;

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
    save_as_overwrite: Option<PathBuf>,
    save_as_error: Option<String>,
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
            save_as_overwrite: None,
            save_as_error: None,
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

    fn highlight_style(&self, style: Style, bg: Color) -> Style {
        theme::highlight_style(self.color_support, style, bg)
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
}

mod editor;
mod files;
mod input;
mod links;
mod mouse;
mod render;
mod search;
mod shortcuts;
mod status;
mod text;

use links::{link_at, open_in_browser};
use shortcuts::*;
use text::*;

#[cfg(test)]
mod tests;
