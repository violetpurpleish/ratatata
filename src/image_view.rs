//! Image previews: decode an image file and render it into the editor pane
//! using the terminal's graphics protocol (kitty, sixel, iTerm2), falling
//! back to unicode half-blocks on terminals with no support.

use std::io;
use std::path::{Path, PathBuf};

use image::ImageReader;
use ratatui::Frame;
use ratatui::layout::{Rect, Size};
use ratatui_image::Image;
use ratatui_image::Resize;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;

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
    /// Picker supplies the terminal graphics protocol and cell dimensions.
    picker: Picker,
    /// Fixed protocol for the last render area.
    protocol: Option<Protocol>,
    protocol_area: Rect,
}

impl ImagePreview {
    /// Decode `path` and prepare a preview for `picker`'s protocol.
    pub fn open(path: PathBuf, picker: &Picker) -> io::Result<Self> {
        let img = decode_image(&path)?;
        let pixels = (img.width(), img.height());
        Ok(Self {
            path,
            pixels,
            image: img,
            picker: picker.clone(),
            protocol: None,
            protocol_area: Rect::default(),
        })
    }

    /// Render the preview contained within `area`.
    ///
    /// The preview preserves the image's aspect ratio and constrains both
    /// dimensions to the editor pane. Large images are scaled to the pane's
    /// limiting dimension using logical cell metrics; genuinely small images
    /// remain at native size. In particular, a tall image is reduced enough
    /// to fit its full height rather than being clipped below the viewport.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        if self.protocol.is_none() || self.protocol_area != area {
            let resize = self.resize_mode(area);
            self.protocol = self
                .picker
                .new_protocol(
                    self.image.clone(),
                    Size::new(area.width, area.height),
                    resize,
                )
                .ok();
            self.protocol_area = area;
        }
        if let Some(protocol) = &self.protocol {
            // The protocol was fitted to this exact area, so leaving clipping
            // disabled guarantees the complete image remains visible.
            frame.render_widget(Image::new(protocol), area);
        }
    }

    /// Use logical terminal cell dimensions for the no-upscaling decision.
    /// The graphics protocol may report backing-pixel dimensions on HiDPI
    /// terminals; using those directly can make a source image look smaller
    /// than the logical editor pane. Scale genuinely large images to the
    /// pane, while retaining native size for small images.
    fn resize_mode(&self, area: Rect) -> Resize {
        const LOGICAL_CELL_WIDTH: u64 = 8;
        const LOGICAL_CELL_HEIGHT: u64 = 16;
        let logical_width = u64::from(area.width) * LOGICAL_CELL_WIDTH;
        let logical_height = u64::from(area.height) * LOGICAL_CELL_HEIGHT;
        if u64::from(self.pixels.0) > logical_width || u64::from(self.pixels.1) > logical_height {
            Resize::Scale(None)
        } else {
            Resize::Fit(None)
        }
    }
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
