//! ratatata — a tiny terminal text editor built with ratatui.
//!
//! Run with no arguments to browse the current directory, or pass a file or
//! directory to open.

mod app;
mod buffer;
mod clipboard;
mod highlight;
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
    let mut app = App::new(dir, file)?;

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
    let result = run(&mut app, &mut terminal);
    let _ = disable_terminal_capabilities();
    ratatui::restore();
    result
}

/// Mouse capture, bracketed paste, and the kitty keyboard protocol (so
/// macOS Cmd+key arrives as `KeyModifiers::SUPER` on supporting terminals;
/// unsupported terminals simply ignore the request).
fn enable_terminal_capabilities() -> io::Result<()> {
    execute!(
        io::stdout(),
        EnableMouseCapture,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
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
         \x20 Ctrl+Q   quit\n\
         \x20 mouse:    click editor to move the cursor, drag to select, double-click\n\
         \x20            selects a word, triple-click selects the line, scroll to move,\n\
         \x20            single-click sidebar to select, double-click to open, scroll to browse\n\
         \x20 sidebar:  arrows/Enter open, Backspace goes up\n\
         \x20 editor:   type, arrows (+Shift to select), Home/End, PgUp/PgDn, Backspace, Delete, Tab"
    );
}
