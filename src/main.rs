//! ratatata — a tiny terminal text editor built with ratatui.
//!
//! Run with no arguments to browse the current directory, or pass a file or
//! directory to open.

mod app;
mod buffer;
mod clipboard;
mod highlight;
mod image_view;
mod search;
mod sidebar;

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::DefaultTerminal;
use ratatui_image::FontSize;
use ratatui_image::picker::Picker;

use app::App;

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_usage();
        return Ok(());
    }
    if args.len() > 1 {
        eprintln!("error: expected at most one path argument");
        print_usage();
        std::process::exit(2);
    }

    let (dir, file) = resolve_start(args.first().map(PathBuf::from))?;

    let mut terminal = match ratatui::try_init() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot initialize terminal: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = enable_terminal_capabilities() {
        eprintln!("warning: cannot enable mouse/paste support: {e}");
    }
    // Query the terminal for graphics-protocol support (kitty, sixel,
    // iTerm2) and its font size so image previews render natively where
    // possible. Terminals without any support fall back to unicode
    // half-blocks. Must run after entering the alternate screen but before
    // the event loop reads input (it briefly reads stdin itself).
    let picker = detect_image_picker();
    let mut app = match App::new(dir, file, picker) {
        Ok(app) => app,
        Err(e) => {
            let _ = disable_terminal_capabilities();
            ratatui::restore();
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let result = run(&mut app, &mut terminal);
    let _ = disable_terminal_capabilities();
    ratatui::restore();
    result
}

/// Mouse capture, bracketed paste, and the kitty keyboard protocol.
///
/// We request DISAMBIGUATE_ESCAPE_CODES + REPORT_EVENT_TYPES so that on
/// supporting terminals (kitty, Ghostty, …) macOS Cmd+key arrives as
/// `KeyModifiers::SUPER` and held keys as Repeat events; unsupported
/// terminals simply ignore the request.
///
/// We deliberately do NOT request REPORT_ALL_KEYS_AS_ESCAPE_CODES. With it,
/// kitty-protocol terminals report printable keys as their *unshifted* base
/// key plus modifiers — e.g. Shift+8 as `8`+SHIFT and Option+5 as `5`+ALT
/// on a German layout — because the shifted character is layout-dependent.
/// crossterm cannot recover it from the event, so typing would insert the
/// base digit instead of the intended character (`(` / `[` …). Without the
/// flag, terminals send the resulting character as plain text while still
/// encoding modifier-only combinations (Ctrl/Cmd+key) as CSI u events,
/// which is what the shortcuts need. REPORT_ALTERNATE_KEYS is kept so
/// terminals that do encode Shift+key as CSI u can attach the shifted
/// character, which crossterm then resolves.
fn enable_terminal_capabilities() -> io::Result<()> {
    execute!(
        io::stdout(),
        EnableMouseCapture,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        )
    )
}

fn disable_terminal_capabilities() -> io::Result<()> {
    execute!(
        io::stdout(),
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableMouseCapture
    )
}

/// Detect graphics support and correct the cell pixel dimensions using the
/// terminal's window-size report when it is available. This matters on
/// HiDPI/Retina terminals: a picker can otherwise receive logical cell
/// dimensions while Kitty image placement uses physical pixels, making an
/// image appear smaller than the actual editor viewport.
#[allow(deprecated)]
fn detect_image_picker() -> Picker {
    let detected = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let protocol = detected.protocol_type();
    let Ok(window) = crossterm::terminal::window_size() else {
        return detected;
    };
    if window.columns == 0 || window.rows == 0 || window.width == 0 || window.height == 0 {
        return detected;
    }

    let font_size = FontSize::new(window.width / window.columns, window.height / window.rows);
    let detected_font = detected.font_size();
    if font_size.width == 0
        || font_size.height == 0
        || (font_size.width == detected_font.width && font_size.height == detected_font.height)
    {
        return detected;
    }

    let mut corrected = Picker::from_fontsize(font_size);
    corrected.set_protocol_type(protocol);
    corrected
}

/// Figure out what directory the sidebar should show and which file (if any)
/// the buffer should open.
fn resolve_start(arg: Option<PathBuf>) -> io::Result<(PathBuf, Option<PathBuf>)> {
    let Some(arg) = arg else {
        return Ok((env::current_dir()?, None));
    };

    if arg.is_dir() {
        return Ok((fs::canonicalize(arg)?, None));
    }
    if arg.exists() {
        let canon = fs::canonicalize(arg)?;
        let dir = canon.parent().unwrap_or(Path::new(".")).to_path_buf();
        return Ok((dir, Some(canon)));
    }

    // Does not exist: treat it as a new file to be created on first save.
    let parent = arg
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let dir = fs::canonicalize(parent)?;
    let name = arg.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid path: {}", arg.display()),
        )
    })?;
    Ok((dir.clone(), Some(dir.join(name))))
}

fn run(app: &mut App, terminal: &mut DefaultTerminal) -> io::Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| app.draw(frame))?;
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                // with the kitty protocol, held keys arrive as Repeat events
                Event::Key(key)
                    if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat =>
                {
                    app.handle_key(key)
                }
                Event::Mouse(event) => app.handle_mouse(event),
                Event::Paste(text) => app.paste_text(text),
                Event::Resize(..) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

fn print_usage() {
    eprintln!(
        "rat — a tiny terminal text editor\n\
         \n\
         usage: rat [path]\n\
         \n\
         Opens `path` if it is a file, or browses it if it is a directory.\n\
         With no argument, the current directory is shown in the sidebar.\n\
         \n\
         keys (Cmd works like Ctrl on macOS-capable terminals):\n\
         \x20 Ctrl+O   switch between sidebar and editor\n\
         \x20 Ctrl+S   save the current file (asks for a name if untitled)\n\
         \x20 Ctrl+Z   undo (Ctrl+Shift+Z redo)\n\
         \x20 Ctrl+C/X/V  copy / cut / paste\n\
         \x20 Ctrl+A   select all\n\
         \x20 Ctrl+F   search (type to filter, Enter/Shift+Enter next/prev, Esc closes)\n\
         \x20 Ctrl+Q   quit\n\
         \x20 mouse:    click editor to move the cursor, drag to select, double-click\n\
         \x20            selects a word, triple-click selects the line, scroll to move,\n\
         \x20            single-click sidebar to select, double-click to open, scroll to browse\n\
         \x20 sidebar:  arrows/Enter open, Backspace goes up\n\
         \x20 editor:   type, arrows (+Shift to select), Home/End, PgUp/PgDn, Backspace, Delete, Tab\n\n         images:  opening an image file (png/jpg/gif/webp/…) previews it in the\n         \x20            editor pane via the terminal's graphics protocol (kitty, sixel,
         \x20            iTerm2, or unicode half-blocks as a last resort); Esc closes"
    );
}
