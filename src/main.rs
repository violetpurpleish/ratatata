//! ratata — a tiny terminal text editor built with ratatui.
//!
//! Run with no arguments to browse the current directory, or pass a file or
//! directory to open.

mod app;
mod buffer;
mod highlight;
mod sidebar;

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
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
    let result = run(&mut app, &mut terminal);
    ratatui::restore();
    result
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
        let dir = canon
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
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
                Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
                Event::Resize(..) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

fn print_usage() {
    eprintln!(
        "ratata — a tiny terminal text editor\n\
         \n\
         usage: ratata [path]\n\
         \n\
         Opens `path` if it is a file, or browses it if it is a directory.\n\
         With no argument, the current directory is shown in the sidebar.\n\
         \n\
         keys:\n\
         \x20 Ctrl+O   switch between sidebar and editor\n\
         \x20 Ctrl+S   save the current file (asks for a name if untitled)\n\
         \x20 Ctrl+Q   quit\n\
         \x20 sidebar: arrows/Enter open, Backspace goes up, Enter on a file opens it\n\
         \x20 editor:  type, arrows, Home/End, PgUp/PgDn, Backspace, Delete, Tab"
    );
}
