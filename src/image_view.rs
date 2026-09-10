//! Image previews: decode an image file and render it into the editor pane
//! using the terminal's graphics protocol (kitty, sixel, iTerm2), falling
//! back to unicode half-blocks on terminals with no support.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::terminal;
use image::ImageReader;
use ratatui::Frame;
use ratatui::layout::{Rect, Size};
use ratatui_image::FontSize;
use ratatui_image::Image;
use ratatui_image::Resize;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;

use crate::theme::{self, ColorSupport};

/// Extensions treated as images and previewed instead of opened as text.
/// These are the formats the `image` crate decodes out of the box.
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tiff", "tif", "pnm", "pbm", "pgm", "ppm",
    "qoi", "tga", "hdr", "exr", "dds",
];

/// Whether `path` looks like an image file (case-insensitive extension check).
pub fn is_image_path(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    IMAGE_EXTENSIONS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(ext))
}

/// A decoded image that replaces the text buffer in the editor pane.
pub struct ImagePreview {
    /// The file being shown.
    pub path: PathBuf,
    /// Decoded pixel dimensions, for the status bar.
    pub pixels: (u32, u32),
    /// Keep the original image so the fixed protocol can be rebuilt on a
    /// terminal resize.
    image: image::DynamicImage,
    /// Picker supplies the terminal graphics protocol and backing-pixel cell
    /// dimensions.
    picker: Picker,
    /// Logical cell dimensions used only for the no-upscaling decision.
    logical_cell_size: FontSize,
    /// Fixed protocol for the last render area.
    protocol: Option<Protocol>,
    protocol_area: Rect,
}

impl ImagePreview {
    /// Decode an image while keeping logical sizing decisions separate from
    /// the picker cell dimensions used by the graphics protocol.
    pub fn open_with_cell_size(
        path: PathBuf,
        picker: &Picker,
        logical_cell_size: FontSize,
    ) -> io::Result<Self> {
        let img = decode_image(&path)?;
        let pixels = (img.width(), img.height());
        Ok(Self {
            path,
            pixels,
            image: img,
            picker: picker.clone(),
            logical_cell_size,
            protocol: None,
            protocol_area: Rect::default(),
        })
    }

    /// Replace terminal metrics without decoding the source image again.
    /// Rendering will rebuild the cached protocol for the new cell size.
    pub fn update_metrics(&mut self, picker: Picker, logical_cell_size: FontSize) {
        self.picker = picker;
        self.logical_cell_size = logical_cell_size;
        self.protocol = None;
        self.protocol_area = Rect::default();
    }

    #[cfg(test)]
    pub(crate) fn has_cached_protocol(&self) -> bool {
        self.protocol.is_some()
    }

    /// Render the preview contained within `area`.
    ///
    /// The preview preserves the image's aspect ratio and constrains both
    /// dimensions to the editor pane. Large images are scaled to the pane's
    /// limiting dimension using the current logical cell metrics; genuinely
    /// small images remain at native size. In particular, a tall image is
    /// reduced enough to fit its full height rather than being clipped below
    /// the viewport.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, color_support: ColorSupport) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        if self.protocol.is_none() || self.protocol_area != area {
            let resize = self.resize_mode(area);
            let requested_size = Size::new(area.width, area.height);
            log_sizing_diagnostics(
                &self.picker,
                self.logical_cell_size,
                &self.image,
                area,
                resize.clone(),
                requested_size,
            );
            self.protocol =
                match self
                    .picker
                    .new_protocol(self.image.clone(), requested_size, resize)
                {
                    Ok(protocol) => {
                        log_protocol_diagnostics(&protocol);
                        Some(protocol)
                    }
                    Err(error) => {
                        append_diagnostics(&format!("protocol_error={error}"));
                        None
                    }
                };
            self.protocol_area = area;
        }
        if let Some(protocol) = &self.protocol {
            // The protocol was fitted to this exact area, so leaving clipping
            // disabled guarantees the complete image remains visible.
            frame.render_widget(Image::new(protocol), area);
            // Half-blocks are ordinary cells with RGB foreground/background
            // pixels. Adapt both after rendering, including cached protocols.
            // Native graphics protocols and their placeholder cells must be
            // left untouched.
            if color_support != ColorSupport::TrueColor
                && matches!(protocol, Protocol::Halfblocks(_))
            {
                let buffer = frame.buffer_mut();
                for y in area.top()..area.bottom() {
                    for x in area.left()..area.right() {
                        if let Some(cell) = buffer.cell_mut((x, y)) {
                            cell.fg = theme::adapt_image_color(color_support, cell.fg);
                            cell.bg = theme::adapt_image_color(color_support, cell.bg);
                        }
                    }
                }
            }
        }
    }

    /// Use logical terminal cell dimensions only for the no-upscaling
    /// decision. The picker retains the reported backing-pixel dimensions for
    /// protocol encoding and placement.
    fn resize_mode(&self, area: Rect) -> Resize {
        resize_mode_for(self.pixels, area, self.logical_cell_size)
    }
}

/// The known-good behavior used when no independent logical-cell information
/// is available. This remains the fallback rather than being replaced by a
/// heuristic.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn fallback_logical_cell_size() -> FontSize {
    FontSize::new(8, 16)
}

fn resize_mode_for(pixels: (u32, u32), area: Rect, cell_size: FontSize) -> Resize {
    let logical_width = u64::from(area.width) * u64::from(cell_size.width);
    let logical_height = u64::from(area.height) * u64::from(cell_size.height);
    if u64::from(pixels.0) > logical_width || u64::from(pixels.1) > logical_height {
        Resize::Scale(None)
    } else {
        Resize::Fit(None)
    }
}

const DIAGNOSTICS_FILE: &str = "ratatata-image-sizing.log";
static DIAGNOSTICS_INITIALIZED: OnceLock<()> = OnceLock::new();

fn diagnostics_enabled() -> bool {
    matches!(std::env::var("RAT_DEBUG_IMAGE").as_deref(), Ok("1"))
}

/// Record a picker before/after terminal-size correction. This deliberately
/// does not classify either value as logical or physical; that is what the
/// runtime comparison is meant to establish.
pub(crate) fn log_picker_observation(label: &str, picker: &Picker) {
    if !diagnostics_enabled() {
        return;
    }
    let font = picker.font_size();
    append_diagnostics(&format!(
        "picker_observation={label} font_size=({}, {}) protocol={:?} capabilities={:?}",
        font.width,
        font.height,
        picker.protocol_type(),
        picker.capabilities(),
    ));
}

pub(crate) fn log_scale_observation(
    physical_cell_size: FontSize,
    backing_scale: Option<f64>,
    logical_cell_size: FontSize,
) {
    if !diagnostics_enabled() {
        return;
    }
    append_diagnostics(&format!(
        "backing_scale={backing_scale:?} physical_cell_size=({}, {}) derived_logical_cell_size=({}, {})",
        physical_cell_size.width,
        physical_cell_size.height,
        logical_cell_size.width,
        logical_cell_size.height,
    ));
}

fn diagnostics_path() -> PathBuf {
    // Keep this discoverable on Unix/macOS instead of hiding it under a
    // per-process TMPDIR path. Windows retains the platform temp directory.
    #[cfg(unix)]
    {
        PathBuf::from("/tmp").join(DIAGNOSTICS_FILE)
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join(DIAGNOSTICS_FILE)
    }
}

fn append_diagnostics(record: &str) {
    if !diagnostics_enabled() {
        return;
    }
    DIAGNOSTICS_INITIALIZED.get_or_init(|| {
        let _ = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(diagnostics_path());
    });
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(diagnostics_path())
    else {
        return;
    };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let _ = writeln!(file, "[{timestamp}] {record}");
}

fn log_sizing_diagnostics(
    picker: &Picker,
    logical_cell_size: FontSize,
    image: &image::DynamicImage,
    area: Rect,
    resize: Resize,
    requested_size: Size,
) {
    if !diagnostics_enabled() {
        return;
    }
    let picker_font = picker.font_size();
    let natural_size = Resize::natural_size(image, picker_font);
    let resized_size = resize.size_for(image, picker_font, requested_size);
    let window = terminal::window_size();
    let window_record = match window {
        Ok(window) => {
            let derived = if window.columns != 0 && window.rows != 0 {
                Some((window.width / window.columns, window.height / window.rows))
            } else {
                None
            };
            format!(
                "window_size columns={} rows={} width={} height={} derived_cell={derived:?}",
                window.columns, window.rows, window.width, window.height
            )
        }
        Err(error) => format!("window_size error={error}"),
    };

    append_diagnostics(&format!(
        "picker_font_size=({}, {}) logical_cell_size=({}, {}) protocol={:?} {window_record} editor_area=(x={}, y={}, width={}, height={}) source_pixels=({}, {}) resize={resize:?} requested_size=({}, {}) natural_size=({}, {}) resize_size_for=({}, {})",
        picker_font.width,
        picker_font.height,
        logical_cell_size.width,
        logical_cell_size.height,
        picker.protocol_type(),
        area.x,
        area.y,
        area.width,
        area.height,
        image.width(),
        image.height(),
        requested_size.width,
        requested_size.height,
        natural_size.width,
        natural_size.height,
        resized_size.width,
        resized_size.height,
    ));
}

fn log_protocol_diagnostics(protocol: &Protocol) {
    if !diagnostics_enabled() {
        return;
    }
    let size = protocol.size();
    append_diagnostics(&format!(
        "protocol_result_size=({}, {})",
        size.width, size.height
    ));
}

/// Decode an image file, mapping decode failures onto `io::Error` so the
/// app's open flows can report them like any other file error.
fn decode_image(path: &Path) -> io::Result<image::DynamicImage> {
    let reader = ImageReader::open(path)?;
    reader.decode().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: not a valid image ({e})", path.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_logical_dimensions_are_trusted_over_the_fallback() {
        let area = Rect::new(0, 0, 100, 30);
        let physical_logical_mode = resize_mode_for((900, 500), area, FontSize::new(16, 34));
        let old_fallback_mode = resize_mode_for((900, 500), area, fallback_logical_cell_size());
        assert!(matches!(physical_logical_mode, Resize::Fit(None)));
        assert!(matches!(old_fallback_mode, Resize::Scale(None)));
    }
}
