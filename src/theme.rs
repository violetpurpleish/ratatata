//! Terminal color capability and the Catppuccin Mocha UI palette.
//!
//! Ghostty and other truecolor terminals keep the RGB theme unchanged.
//! Capability is detected from what the terminal *advertises* (`COLORTERM`
//! / equivalent), never from `NO_COLOR` or `FORCE_COLOR` — those are
//! sandbox and CI conventions, not a signal that RGB is unsupported.

use ratatui::style::{Color, Modifier, Style};
use ratatui_themes::ThemePalette;

/// The color depth advertised by the terminal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorSupport {
    /// Use Catppuccin Mocha RGB exactly as the truecolor theme specifies.
    TrueColor,
    /// Approximate RGB with the xterm color cube and grayscale ramp.
    Indexed256,
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
    } else if term.is_some_and(|term| term.trim().split('-').any(|part| part == "256color")) {
        ColorSupport::Indexed256
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

/// Adapt application and syntax colors to the advertised color depth.
///
/// Catppuccin Mocha colors map to deliberate ANSI 16 slots: nearest RGB
/// matching alone turns its pastel keywords, functions and strings gray.
/// Images use `adapt_image_color` instead, without these theme substitutions.
pub fn adapt_color(support: ColorSupport, color: Color) -> Color {
    match support {
        ColorSupport::TrueColor => color,
        ColorSupport::Indexed256 => adapt_image_color(support, color),
        ColorSupport::Ansi16 => to_ansi16(color),
    }
}

/// Quantize image pixels without interpreting their colors as theme roles.
pub fn adapt_image_color(support: ColorSupport, color: Color) -> Color {
    match (support, color) {
        (ColorSupport::Indexed256, Color::Rgb(r, g, b)) => nearest_256(r, g, b),
        (ColorSupport::Ansi16, Color::Rgb(r, g, b)) => nearest_ansi16(r, g, b),
        (ColorSupport::Ansi16, Color::Indexed(index)) => {
            let (r, g, b) = indexed_rgb(index);
            nearest_ansi16(r, g, b)
        }
        _ => color,
    }
}

/// Limited palettes use a contrasting foreground on highlighted text.
/// ANSI selections use explicit white-on-blue instead of inheriting token
/// colors; dim hidden-file styling is removed while selected. Truecolor is unchanged.
pub fn highlight_style(support: ColorSupport, style: Style, bg: Color) -> Style {
    let style = adapt_style(support, style).bg(adapt_color(support, bg));
    if support == ColorSupport::TrueColor {
        return style;
    }
    let foreground = match style.bg {
        Some(Color::Indexed(index)) => {
            let (r, g, b) = indexed_rgb(index);
            let linear = |c: u8| {
                let c = f64::from(c) / 255.0;
                if c <= 0.04045 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            let luminance = 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
            // Choose whichever has the higher contrast ratio. Use extended
            // slots so a customized ANSI black/white cannot change the result.
            if luminance > 0.179 {
                Color::Indexed(16)
            } else {
                Color::Indexed(231)
            }
        }
        Some(Color::LightYellow) => Color::Black,
        _ => Color::White,
    };
    style.fg(foreground).remove_modifier(Modifier::DIM)
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
        ColorSupport::Indexed256 => ThemePalette {
            accent: adapt_color(support, rgb.accent),
            secondary: adapt_color(support, rgb.secondary),
            bg: adapt_color(support, rgb.bg),
            fg: adapt_color(support, rgb.fg),
            muted: adapt_color(support, rgb.muted),
            selection: adapt_color(support, rgb.selection),
            error: adapt_color(support, rgb.error),
            warning: adapt_color(support, rgb.warning),
            success: adapt_color(support, rgb.success),
            info: adapt_color(support, rgb.info),
        },
        ColorSupport::Ansi16 => ThemePalette {
            accent: Color::LightBlue,
            secondary: Color::LightMagenta,
            bg: Color::Black,
            fg: Color::White,
            muted: Color::DarkGray,
            selection: Color::Blue,
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
        // Catppuccin Mocha accent families, shared by the UI and syntect.
        Color::Rgb(245, 224, 220) => Color::White, // rosewater
        Color::Rgb(242, 205, 205) | Color::Rgb(243, 139, 168) | Color::Rgb(235, 160, 172) => {
            Color::LightRed
        }
        Color::Rgb(245, 194, 231) | Color::Rgb(203, 166, 247) => Color::LightMagenta,
        Color::Rgb(250, 179, 135) | Color::Rgb(249, 226, 175) => Color::LightYellow,
        Color::Rgb(166, 227, 161) => Color::LightGreen,
        Color::Rgb(148, 226, 213) | Color::Rgb(137, 220, 235) | Color::Rgb(116, 199, 236) => {
            Color::LightCyan
        }
        Color::Rgb(137, 180, 250) | Color::Rgb(180, 190, 254) => Color::LightBlue,
        Color::Rgb(205, 214, 244) => Color::White,
        Color::Rgb(186, 194, 222) | Color::Rgb(166, 173, 200) => Color::Gray,
        Color::Rgb(147, 153, 178)
        | Color::Rgb(127, 132, 156)
        | Color::Rgb(108, 112, 134)
        | Color::Rgb(88, 91, 112)
        | Color::Rgb(69, 71, 90)
        | Color::Rgb(49, 50, 68) => Color::DarkGray,
        Color::Rgb(30, 30, 46) | Color::Rgb(24, 24, 37) | Color::Rgb(17, 17, 27) => Color::Black,
        other => adapt_image_color(ColorSupport::Ansi16, other),
    }
}

/// Nominal ANSI colors used to approximate image pixels and unknown colors.
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

const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn indexed_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        0..=15 => {
            let (r, g, b, _) = ANSI16_RGB[index as usize];
            (r, g, b)
        }
        16..=231 => {
            let n = usize::from(index - 16);
            (
                CUBE_LEVELS[n / 36],
                CUBE_LEVELS[n / 6 % 6],
                CUBE_LEVELS[n % 6],
            )
        }
        _ => {
            let value = 8 + 10 * (index - 232);
            (value, value, value)
        }
    }
}

fn color_distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let dr = i32::from(a.0) - i32::from(b.0);
    let dg = i32::from(a.1) - i32::from(b.1);
    let db = i32::from(a.2) - i32::from(b.2);
    (dr * dr + dg * dg + db * db) as u32
}

/// Compare the nearest cube entry with the nearest gray. Exclude ANSI slots
/// 0–15 because users can customize those independently of the extended table.
fn nearest_256(r: u8, g: u8, b: u8) -> Color {
    let component = |value: u8| -> u8 {
        CUBE_LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, level)| value.abs_diff(**level))
            .unwrap()
            .0 as u8
    };
    let cube = 16 + 36 * component(r) + 6 * component(g) + component(b);
    let mean = (i32::from(r) + i32::from(g) + i32::from(b)) / 3;
    let gray = 232 + ((mean - 8 + 5) / 10).clamp(0, 23) as u8;
    let rgb = (r, g, b);
    Color::Indexed(
        if color_distance(rgb, indexed_rgb(gray)) < color_distance(rgb, indexed_rgb(cube)) {
            gray
        } else {
            cube
        },
    )
}

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
    fn advertised_256_colors_are_preserved_without_truecolor() {
        for term in [
            "xterm-256color",
            "screen-256color",
            "tmux-256color",
            "rxvt-unicode-256color",
            "screen-256color-bce",
        ] {
            for colorterm in [None, Some(""), Some("yes")] {
                assert_eq!(
                    detect_color_support_from_env(colorterm, Some(term)),
                    ColorSupport::Indexed256
                );
            }
        }
    }

    #[test]
    fn unadvertised_color_depth_stays_conservative() {
        for term in [
            None,
            Some("xterm"),
            Some("linux"),
            Some("screen"),
            Some("dumb"),
        ] {
            assert_eq!(
                detect_color_support_from_env(None, term),
                ColorSupport::Ansi16
            );
        }
    }

    #[test]
    fn indexed_palette_preserves_pastels_and_dark_backgrounds() {
        for (rgb, index) in [
            ((203, 166, 247), 183), // keyword: mauve
            ((137, 180, 250), 111), // function: blue
            ((166, 227, 161), 151), // string: green
            ((30, 30, 46), 235),    // base: dark gray, not black
            ((0, 0, 0), 16),
            ((255, 255, 255), 231),
            ((128, 128, 128), 244),
        ] {
            assert_eq!(
                adapt_color(ColorSupport::Indexed256, Color::Rgb(rgb.0, rgb.1, rgb.2)),
                Color::Indexed(index)
            );
        }
        for index in 16..=255 {
            let (r, g, b) = indexed_rgb(index);
            assert_eq!(
                adapt_image_color(ColorSupport::Indexed256, Color::Rgb(r, g, b)),
                Color::Indexed(index)
            );
        }
    }

    #[test]
    fn image_quantization_does_not_apply_syntax_color_substitutions() {
        let pastel = Color::Rgb(203, 166, 247);
        assert_eq!(
            adapt_color(ColorSupport::Ansi16, pastel),
            Color::LightMagenta
        );
        assert_eq!(adapt_image_color(ColorSupport::Ansi16, pastel), Color::Gray);
        assert_eq!(
            adapt_image_color(ColorSupport::Ansi16, Color::Indexed(196)),
            Color::LightRed
        );
        assert_eq!(adapt_image_color(ColorSupport::TrueColor, pastel), pastel);
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
        assert_eq!(p.selection, Color::Blue);
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
    fn ansi16_syntax_colors_keep_their_accent_families() {
        for (rgb, expected) in [
            (Color::Rgb(203, 166, 247), Color::LightMagenta),
            (Color::Rgb(137, 180, 250), Color::LightBlue),
            (Color::Rgb(166, 227, 161), Color::LightGreen),
            (Color::Rgb(249, 226, 175), Color::LightYellow),
            (Color::Rgb(243, 139, 168), Color::LightRed),
            (Color::Rgb(148, 226, 213), Color::LightCyan),
            (Color::Rgb(108, 112, 134), Color::DarkGray),
        ] {
            assert_eq!(adapt_color(ColorSupport::Ansi16, rgb), expected);
        }
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
