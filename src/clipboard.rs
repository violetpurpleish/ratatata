//! OS clipboard access for copy/cut/paste.
//!
//! The [`Clipboard`] trait is used so tests can substitute an in-memory
//! fake; the real implementation talks to the system clipboard via
//! `arboard`. If the clipboard is unavailable (headless session, sandbox),
//! all operations become silent no-ops instead of crashing.

pub trait Clipboard {
    /// Current clipboard contents, if any.
    fn get_text(&mut self) -> Option<String>;
    /// Replace the clipboard contents.
    fn set_text(&mut self, text: &str);
}

/// System clipboard backed by `arboard`.
pub struct SystemClipboard {
    inner: Option<arboard::Clipboard>,
}

impl SystemClipboard {
    pub fn new() -> Self {
        Self {
            inner: arboard::Clipboard::new().ok(),
        }
    }
}

impl Default for SystemClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Clipboard for SystemClipboard {
    fn get_text(&mut self) -> Option<String> {
        self.inner.as_mut().and_then(|c| c.get_text().ok())
    }

    fn set_text(&mut self, text: &str) {
        if let Some(clipboard) = &mut self.inner {
            let _ = clipboard.set_text(text);
        }
    }
}
