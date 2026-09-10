//! Shortcut labels, descriptions, styles, and top-bar layout.

use super::*;

/// The actions behind the keyboard shortcuts and the clickable buttons in
/// the top bar. Both input paths route through [`App::invoke_shortcut`], so
/// the keys and the mouse always behave identically.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Shortcut {
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
    pub(super) fn key_label(self) -> &'static str {
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
    pub(super) fn action_label(self) -> &'static str {
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
    pub(super) fn description(self) -> &'static str {
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
    pub(super) fn key_color(self) -> Color {
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

/// Rendered width of one shortcut button ("Ctrl+O switch").
pub(super) fn pill_width(action: Shortcut) -> u16 {
    (action.key_label().width() + action.action_label().width() + 1) as u16
}

/// The spans of one shortcut button: the key combo in bold accent color
/// followed by the action name. Only the hovered button gets a background
/// and brighter label; idle buttons blend into the top bar.
pub(super) fn pill_spans(
    action: Shortcut,
    hovered: bool,
    color_support: ColorSupport,
) -> Vec<Span<'static>> {
    let pal = theme::ui_palette(color_support, PALETTE);
    let hover_bg = if color_support == ColorSupport::Ansi16 {
        pal.selection
    } else {
        theme::adapt_color(color_support, TOPBAR_PILL_BG_HOVER)
    };
    let mut key_style = Style::default()
        .fg(theme::adapt_color(color_support, action.key_color()))
        .add_modifier(Modifier::BOLD);
    let mut label_style = Style::default().fg(if hovered { pal.fg } else { pal.muted });
    if hovered {
        key_style = theme::highlight_style(color_support, key_style, hover_bg);
        label_style = theme::highlight_style(color_support, label_style, hover_bg);
    }
    vec![
        Span::styled(format!("{} ", action.key_label()), key_style),
        Span::styled(action.action_label(), label_style),
    ]
}

/// How many rows the top bar needs at `width`. Must stay in sync with
/// [`App::draw_topbar`] via [`wrap_topbar_pills`].
pub(super) fn shortcut_bar_height(pills: &[Shortcut], width: u16) -> u16 {
    wrap_topbar_pills(pills, width)
        .pills
        .last()
        .map(|p| p.row + 1)
        .unwrap_or(1)
        .max(1)
}

/// One shortcut button after wrapping.
#[derive(Clone, Copy, Debug)]
pub(super) struct PlacedPill {
    pub(super) action: Shortcut,
    pub(super) x: u16,
    pub(super) row: u16,
    pub(super) width: u16,
}

pub(super) struct TopbarLayout {
    pub(super) pills: Vec<PlacedPill>,
    pub(super) truncated: bool,
}

/// Wrap shortcut buttons into rows. A trailing Quit is pinned: it is always
/// placed, wrapping onto a new row if needed, and the last allowed body row
/// reserves space for it so it cannot be the button eaten by the ellipsis.
pub(super) fn wrap_topbar_pills(pills: &[Shortcut], width: u16) -> TopbarLayout {
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
