//! Shared fixtures for the behavior regression suite.

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

    fn set_text(&mut self, text: &str) -> Result<(), String> {
        self.text = text.to_string();
        Ok(())
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

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp")
        .join(format!("ratatata-app-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_search_typed(app: &mut App, query: &str) {
    app.handle_key(ctrl('f'));
    for c in query.chars() {
        app.handle_key(char_key(c));
    }
}

use ratatui::Terminal;
use ratatui::backend::TestBackend;

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

fn ctrl_shift(c: char) -> KeyEvent {
    KeyEvent::new(
        KeyCode::Char(c),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    )
}

fn cmd_shift(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::SUPER | KeyModifiers::SHIFT)
}

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

fn parinfer_two_forms_app(name: &str) -> App {
    let dir = scratch(name);
    let file = dir.join("core.clj");
    fs::write(&file, "(foo)\n(bar)").unwrap();
    new_app(dir, Some(file)).unwrap()
}

struct FailingClipboard;
impl Clipboard for FailingClipboard {
    fn get_text(&mut self) -> Option<String> {
        None
    }
    fn set_text(&mut self, _: &str) -> Result<(), String> {
        Err("unavailable".into())
    }
}

mod clipboard;
mod files;
mod history;
mod images;
mod input;
mod mouse;
mod parinfer;
mod rendering;
mod search;
