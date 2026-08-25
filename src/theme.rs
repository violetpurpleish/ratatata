//! Terminal color capability and the Catppuccin Mocha UI palette.
//!
//! Ghostty and other truecolor terminals keep the RGB theme unchanged.
//! Capability is detected from what the terminal *advertises* (`COLORTERM`
//! / equivalent), never from `NO_COLOR` or `FORCE_COLOR` — those are
//! sandbox and CI conventions, not a signal that RGB is unsupported.

use ratatui::style::{Color, Modifier, Style};
use ratatui_themes::ThemePalette;

/// Whether the terminal advertised 24-bit color.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorSupport {
    /// Use Catppuccin Mocha RGB exactly as the truecolor theme specifies.
    TrueColor,
    /// Map the same semantic palette onto the ANSI 16-color set.
    Ansi16,
}

/// Detect color support from the process environment.
///
/// `NO_COLOR` and `FORCE_COLOR` are deliberately ignored so a truecolor
/// terminal (Ghostty with `COLORTERM=truecolor`) keeps the RGB theme even
/// when a sandbox exported those variables.
pub fn detect_color_support() -> ColorSupport {
    detect_color_support_from_env(
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    )
}

/// Truecolor is advertised by `COLORTERM=truecolor` / `24bit`, or by a
/// terminfo family whose name ends in `-direct` (the 24-bit convention).
pub fn detect_color_support_from_env(colorterm: Option<&str>, term: Option<&str>) -> ColorSupport {
    if truecolor_advertised(colorterm, term) {
        ColorSupport::TrueColor
    } else {
        ColorSupport::Ansi16
    }
}

pub fn truecolor_advertised(colorterm: Option<&str>, term: Option<&str>) -> bool {
    if let Some(colorterm) = colorterm {
        let ct = colorterm.trim().to_ascii_lowercase();
        if ct == "truecolor" || ct == "24bit" {
            return true;
        }
    }
    if let Some(term) = term {
        let term = term.trim();
        if term.ends_with("-direct") {
            return true;
        }
    }
    false
}

/// Identity on the truecolor path; RGB → ANSI 16 otherwise.
///
/// Semantic palette colors (the Catppuccin Mocha UI theme) map to fixed
/// ANSI 16 slots so the focus border stays a distinct color. Any other
/// RGB value (syntax highlighting) is mapped to the nearest ANSI 16 color.
pub fn adapt_color(support: ColorSupport, color: Color) -> Color {
    match support {
        ColorSupport::TrueColor => color,
        ColorSupport::Ansi16 => to_ansi16(color),
    }
}

pub fn adapt_style(support: ColorSupport, mut style: Style) -> Style {
    if support == ColorSupport::TrueColor {
        return style;
    }
    if let Some(fg) = style.fg {
        style.fg = Some(adapt_color(support, fg));
    }
    if let Some(bg) = style.bg {
        style.bg = Some(adapt_color(support, bg));
    }
    style
}

/// The UI palette for `support`. Truecolor returns `rgb` unchanged.
pub fn ui_palette(support: ColorSupport, rgb: ThemePalette) -> ThemePalette {
    match support {
        ColorSupport::TrueColor => rgb,
        ColorSupport::Ansi16 => ThemePalette {
            accent: Color::LightBlue,
            secondary: Color::LightMagenta,
            bg: Color::Black,
            fg: Color::White,
            muted: Color::DarkGray,
            selection: Color::DarkGray,
            error: Color::LightRed,
            warning: Color::LightYellow,
            success: Color::LightGreen,
            info: Color::LightCyan,
        },
    }
}

/// A colored focus border is invisible when it matches the background
/// (including Reset/Black, which many 16-color dark terminals treat as
/// "no color"). Truecolor never takes this fallback.
pub fn colored_border_invisible(border: Color, bg: Color) -> bool {
    if border == bg {
        return true;
    }
    matches!(
        (border, bg),
        (Color::Reset, Color::Black) | (Color::Black, Color::Reset) | (Color::Reset, Color::Reset)
    )
}

/// Reverse/bold the focused pane title when a colored border would vanish.
pub fn focus_title_needs_emphasis(support: ColorSupport, rgb: ThemePalette) -> bool {
    if support == ColorSupport::TrueColor {
        return false;
    }
    let p = ui_palette(support, rgb);
    colored_border_invisible(p.accent, p.bg)
}

/// Title style and border style for a pane that currently has focus.
pub fn focused_pane_styles(
    support: ColorSupport,
    rgb: ThemePalette,
    title_fg: Color,
) -> (Style, Style) {
    let p = ui_palette(support, rgb);
    let title_fg = adapt_color(support, title_fg);
    let mut title = Style::default().fg(title_fg).add_modifier(Modifier::BOLD);
    let border = if focus_title_needs_emphasis(support, rgb) {
        title = title.add_modifier(Modifier::REVERSED);
        Style::default()
    } else {
        Style::default().fg(p.accent)
    };
    (title, border)
}

/// Unfocused pane chrome: muted border, bold title in `title_fg`.
pub fn unfocused_pane_styles(
    support: ColorSupport,
    rgb: ThemePalette,
    title_fg: Color,
) -> (Style, Style) {
    let p = ui_palette(support, rgb);
    let title = Style::default()
        .fg(adapt_color(support, title_fg))
        .add_modifier(Modifier::BOLD);
    (title, Style::default().fg(p.muted))
}

fn to_ansi16(color: Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => nearest_ansi16(r, g, b),
        other => other,
    }
}

/// VGA-ish ANSI 16 primaries used for nearest-neighbour mapping of syntax RGB.
const ANSI16_RGB: [(u8, u8, u8, Color); 16] = [
    (0, 0, 0, Color::Black),
    (128, 0, 0, Color::Red),
    (0, 128, 0, Color::Green),
    (128, 128, 0, Color::Yellow),
    (0, 0, 128, Color::Blue),
    (128, 0, 128, Color::Magenta),
    (0, 128, 128, Color::Cyan),
    (192, 192, 192, Color::Gray),
    (128, 128, 128, Color::DarkGray),
    (255, 0, 0, Color::LightRed),
    (0, 255, 0, Color::LightGreen),
    (255, 255, 0, Color::LightYellow),
    (0, 0, 255, Color::LightBlue),
    (255, 0, 255, Color::LightMagenta),
    (0, 255, 255, Color::LightCyan),
    (255, 255, 255, Color::White),
];

fn nearest_ansi16(r: u8, g: u8, b: u8) -> Color {
    let mut best = ANSI16_RGB[0].3;
    let mut best_d = u32::MAX;
    for &(ar, ag, ab, color) in &ANSI16_RGB {
        let dr = i32::from(r) - i32::from(ar);
        let dg = i32::from(g) - i32::from(ag);
        let db = i32::from(b) - i32::from(ab);
        let d = (dr * dr + dg * dg + db * db) as u32;
        if d < best_d {
            best_d = d;
            best = color;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_themes::{Theme, ThemeName};

    const RGB: ThemePalette = Theme::new(ThemeName::CatppuccinMocha).palette();

    #[test]
    fn ghostty_colorterm_is_truecolor() {
        assert_eq!(
            detect_color_support_from_env(Some("truecolor"), Some("xterm-ghostty")),
            ColorSupport::TrueColor
        );
    }

    #[test]
    fn colorterm_24bit_is_truecolor() {
        assert_eq!(
            detect_color_support_from_env(Some("24bit"), Some("xterm-256color")),
            ColorSupport::TrueColor
        );
    }

    #[test]
    fn term_direct_is_truecolor_without_colorterm() {
        assert_eq!(
            detect_color_support_from_env(None, Some("xterm-direct")),
            ColorSupport::TrueColor
        );
    }

    #[test]
    fn xfce4_style_256color_is_ansi16() {
        // xfce4-terminal: no COLORTERM=truecolor, TERM=xterm-256color.
        assert_eq!(
            detect_color_support_from_env(None, Some("xterm-256color")),
            ColorSupport::Ansi16
        );
        assert_eq!(
            detect_color_support_from_env(Some(""), Some("xterm-256color")),
            ColorSupport::Ansi16
        );
    }

    #[test]
    fn truecolor_detection_does_not_read_no_color() {
        // The detector is not passed NO_COLOR; advertising truecolor is
        // enough. This documents that sandbox NO_COLOR=1 must not win.
        assert!(truecolor_advertised(
            Some("truecolor"),
            Some("xterm-ghostty")
        ));
        assert_eq!(
            detect_color_support_from_env(Some("truecolor"), Some("xterm-ghostty")),
            ColorSupport::TrueColor
        );
    }

    #[test]
    fn truecolor_keeps_catppuccin_rgb_unchanged() {
        let p = ui_palette(ColorSupport::TrueColor, RGB);
        assert_eq!(p, RGB);
        assert_eq!(
            adapt_color(ColorSupport::TrueColor, RGB.accent),
            Color::Rgb(137, 180, 250)
        );
        assert_eq!(
            adapt_color(ColorSupport::TrueColor, RGB.bg),
            Color::Rgb(30, 30, 46)
        );
    }

    #[test]
    fn ansi16_maps_semantic_palette_to_named_colors() {
        let p = ui_palette(ColorSupport::Ansi16, RGB);
        assert_eq!(p.bg, Color::Black);
        assert_eq!(p.fg, Color::White);
        assert_eq!(p.accent, Color::LightBlue);
        assert_eq!(p.muted, Color::DarkGray);
        assert_eq!(p.selection, Color::DarkGray);
        assert_eq!(p.error, Color::LightRed);
        assert_eq!(p.warning, Color::LightYellow);
        assert_eq!(p.success, Color::LightGreen);
        assert_eq!(p.info, Color::LightCyan);
        assert_eq!(p.secondary, Color::LightMagenta);
        // named colors are already ANSI: adapt_color must not wrap them again
        assert_eq!(
            adapt_color(ColorSupport::Ansi16, Color::LightBlue),
            Color::LightBlue
        );
    }

    #[test]
    fn ansi16_syntax_rgb_maps_to_a_named_color() {
        // mauve-ish keyword from Catppuccin Mocha highlighting
        let mapped = adapt_color(ColorSupport::Ansi16, Color::Rgb(203, 166, 247));
        assert!(
            !matches!(mapped, Color::Rgb(..)),
            "syntax RGB must not stay RGB on ANSI 16, got {mapped:?}"
        );
    }

    #[test]
    fn mocha_ansi_focus_border_is_visible() {
        let p = ui_palette(ColorSupport::Ansi16, RGB);
        assert!(!colored_border_invisible(p.accent, p.bg));
        assert!(!focus_title_needs_emphasis(ColorSupport::Ansi16, RGB));
        let (title, border) = focused_pane_styles(ColorSupport::Ansi16, RGB, RGB.success);
        assert_eq!(border.fg, Some(Color::LightBlue));
        assert!(!title.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn truecolor_focus_border_stays_rgb_accent() {
        let (title, border) = focused_pane_styles(ColorSupport::TrueColor, RGB, RGB.success);
        assert_eq!(border.fg, Some(RGB.accent));
        assert_eq!(title.fg, Some(RGB.success));
        assert!(title.add_modifier.contains(Modifier::BOLD));
        assert!(!title.add_modifier.contains(Modifier::REVERSED));
        assert!(!focus_title_needs_emphasis(ColorSupport::TrueColor, RGB));
    }

    #[test]
    fn invisible_border_uses_reverse_bold_title() {
        assert!(colored_border_invisible(Color::Black, Color::Black));
        assert!(colored_border_invisible(Color::Reset, Color::Black));
        assert!(!colored_border_invisible(Color::LightBlue, Color::Black));

        let mut title = Style::default()
            .fg(Color::LightGreen)
            .add_modifier(Modifier::BOLD);
        if colored_border_invisible(Color::Black, Color::Black) {
            title = title.add_modifier(Modifier::REVERSED);
        }
        assert!(title.add_modifier.contains(Modifier::REVERSED));
        assert!(title.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn adapt_style_is_identity_on_truecolor() {
        let style = Style::default().fg(RGB.accent).bg(RGB.bg);
        assert_eq!(adapt_style(ColorSupport::TrueColor, style), style);
    }
}
