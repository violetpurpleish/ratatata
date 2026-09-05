//! ratatata — a tiny terminal text editor built with ratatui.
//!
//! Run with no arguments to browse the current directory, or pass a file or
//! directory to open.

mod app;
mod buffer;
mod clipboard;
mod highlight;
mod image_view;
mod parinfer;
mod parinfer_engine;
mod search;
mod sidebar;
mod theme;

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// Restores terminal state when the application exits, including during panic
/// unwinding.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_usage();
        return Ok(());
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
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
    let _terminal_guard = TerminalGuard;

    if let Err(e) = enable_terminal_capabilities() {
        eprintln!("warning: cannot enable mouse/paste support: {e}");
    }
    // Query the terminal for graphics-protocol support (kitty, sixel,
    // iTerm2) and its font size so image previews render natively where
    // possible. Terminals without any support fall back to unicode
    // half-blocks. Must run after entering the alternate screen but before
    // the event loop reads input (it briefly reads stdin itself).
    let (picker, logical_cell_size) = detect_image_picker();
    let mut app = App::new_with_cell_size(dir, file, picker, logical_cell_size)?;
    app.set_color_support(theme::detect_color_support());
    run(&mut app, &mut terminal)
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
    // Try every mode independently. On platforms where one command is not
    // supported, executing the commands as one crossterm batch would stop at
    // that command and could leave mouse capture enabled.
    let mut stdout = io::stdout();
    let mut first_error = None;

    for result in [
        execute!(&mut stdout, PopKeyboardEnhancementFlags),
        execute!(&mut stdout, DisableBracketedPaste),
        execute!(&mut stdout, DisableMouseCapture),
    ] {
        if let Err(error) = result
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }

    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Return the terminal to the shell without leaving input generated while
/// mouse capture was enabled in its input queue. If those reports are left
/// unread, the shell interprets their SGR escape sequences as user input and
/// prints fragments such as `34;88;2M` at the prompt.
fn restore_terminal() {
    let _ = disable_terminal_capabilities();
    drain_pending_events();
    ratatui::restore();
}

/// Mouse reports can already be buffered when the quit event is handled. Stop
/// tracking first, then let crossterm consume everything that was queued while
/// the application still owned the terminal. This must happen before raw mode
/// is disabled by [`ratatui::restore`].
///
/// The grace period is intentional. A mouse click on the Quit button causes
/// the app to exit on the button-down report, while the terminal may still
/// send the matching button-up report. Also, crossterm's zero-duration poll
/// does not reliably inspect events already buffered in its parser. Keep raw
/// mode enabled and use short, positive polls so both cases are consumed
/// before the shell gets control of stdin again.
fn drain_pending_events() {
    const DRAIN_WINDOW: Duration = Duration::from_millis(50);
    const POLL_INTERVAL: Duration = Duration::from_millis(5);
    let deadline = Instant::now() + DRAIN_WINDOW;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        match event::poll(remaining.min(POLL_INTERVAL)) {
            Ok(true) => {
                if event::read().is_err() {
                    break;
                }
            }
            Ok(false) => {}
            Err(_) => break,
        }
    }
}

/// Detect graphics support and retain the picker cell dimensions used by the
/// graphics protocol. On macOS, an AppKit backing-scale query supplies the
/// separate logical cell dimensions used only by the resize decision.
#[allow(deprecated)]
fn detect_image_picker() -> (Picker, FontSize) {
    let detected = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    image_view::log_picker_observation("query", &detected);
    refresh_image_picker(&detected)
}

/// Refresh the terminal cell metrics without querying graphics capabilities
/// again. The existing protocol type is copied to any newly constructed
/// picker, which is important during a font-size-only terminal resize.
#[allow(deprecated)]
pub(crate) fn refresh_image_picker(current: &Picker) -> (Picker, FontSize) {
    let protocol = current.protocol_type();
    let picker = match crossterm::terminal::window_size() {
        Ok(window)
            if window.columns != 0
                && window.rows != 0
                && window.width != 0
                && window.height != 0 =>
        {
            let font_size =
                FontSize::new(window.width / window.columns, window.height / window.rows);
            let current_font = current.font_size();
            if font_size.width != 0
                && font_size.height != 0
                && (font_size.width != current_font.width
                    || font_size.height != current_font.height)
            {
                let mut refreshed = Picker::from_fontsize(font_size);
                refreshed.set_protocol_type(protocol);
                refreshed
            } else {
                current.clone()
            }
        }
        _ => current.clone(),
    };

    image_view::log_picker_observation("render", &picker);
    let physical_cell_size = picker.font_size();
    let backing_scale = macos_backing_scale_factor();
    let logical_cell_size = logical_cell_size_for_platform(physical_cell_size, backing_scale);
    image_view::log_scale_observation(physical_cell_size, backing_scale, logical_cell_size);
    (picker, logical_cell_size)
}

/// Convert backing-pixel cell dimensions into logical dimensions. Rounding is
/// necessary because ratatui-image represents cell dimensions as integers.
fn logical_cell_size_for_platform(
    physical_cell_size: FontSize,
    backing_scale: Option<f64>,
) -> FontSize {
    #[cfg(target_os = "macos")]
    {
        macos_logical_cell_size_or_fallback(physical_cell_size, backing_scale)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = backing_scale;
        non_macos_logical_cell_size(physical_cell_size)
    }
}

#[cfg(target_os = "macos")]
fn macos_logical_cell_size_or_fallback(
    physical_cell_size: FontSize,
    backing_scale: Option<f64>,
) -> FontSize {
    logical_cell_size_from_scale(physical_cell_size, backing_scale)
        .unwrap_or_else(image_view::fallback_logical_cell_size)
}

#[cfg(any(not(target_os = "macos"), test))]
fn non_macos_logical_cell_size(physical_cell_size: FontSize) -> FontSize {
    physical_cell_size
}

/// Convert backing-pixel cell dimensions into logical dimensions. Rounding is
/// necessary because ratatui-image represents cell dimensions as integers.
#[cfg(any(target_os = "macos", test))]
fn logical_cell_size_from_scale(
    backing_cell_size: FontSize,
    backing_scale: Option<f64>,
) -> Option<FontSize> {
    let scale = backing_scale?;
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }

    fn divide(value: u16, scale: f64) -> Option<u16> {
        let logical = f64::from(value) / scale;
        if !logical.is_finite() || logical < 1.0 || logical > f64::from(u16::MAX) {
            return None;
        }
        Some(logical.round() as u16)
    }

    Some(FontSize::new(
        divide(backing_cell_size.width, scale)?,
        divide(backing_cell_size.height, scale)?,
    ))
}

#[cfg(target_os = "macos")]
fn macos_backing_scale_factor() -> Option<f64> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;

    let marker = MainThreadMarker::new()?;
    let screen = NSScreen::mainScreen(marker)?;
    let scale = screen.backingScaleFactor();
    (scale.is_finite() && scale > 0.0).then_some(scale)
}

#[cfg(not(target_os = "macos"))]
fn macos_backing_scale_factor() -> Option<f64> {
    None
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
                Event::Resize(..) => app.refresh_terminal_metrics(),
                _ => {}
            }
        }
    }
    Ok(())
}

fn print_usage() {
    eprint!("{}", usage_text());
}

fn usage_text() -> String {
    format!(
        "rat — a tiny terminal text editor\n\
         \n\
         usage: rat [path]\n\
         \x20      rat -h | --help\n\
         \x20      rat -V | --version\n\
         \n\
         Opens `path` if it is a file, or browses it if it is a directory.\n\
         A path that does not exist yet opens an empty buffer; the file is\n\
         created on the first save.\n\
         With no argument, the current directory is shown in the sidebar.\n\
         \n\
         \x20 -h, --help     Show this help and exit\n\
         \x20 -V, --version  Print the version and exit\n\
         \n\
         keys (Cmd works like Ctrl on macOS-capable terminals):\n\
         \x20 Ctrl+N   start a new untitled buffer\n\
         \x20 Ctrl+O   switch between sidebar and editor\n\
         \x20 Ctrl+B   show or hide the file sidebar\n\
         \x20 Ctrl+S   save the current file (asks for a name if untitled)\n\
         \x20 Ctrl+R   reload the current file from disk (refreshes the sidebar too)\n\
         \x20 Ctrl+Z   undo (Ctrl+Shift+Z redo)\n\
         \x20 Ctrl+C/X/V  copy / cut / paste\n\
         \x20 Ctrl+A   select all\n\
         \x20 Ctrl+F   search (type to filter, Enter/Shift+Enter next/prev, Esc closes)\n\
         \x20 Ctrl+Shift+H  find and replace (Tab field, Enter replace, Shift+Enter all, Esc closes)\n\
         \x20 Ctrl+G   go to line\n\
         \x20 Ctrl+W   toggle word wrapping of long lines\n\
         \x20 Ctrl+H   show or hide dotfiles in the sidebar\n\
         \x20 Ctrl+Q   quit\n\
         \x20 mouse:    click editor to move the cursor, Ctrl/Cmd+click a web link to\n\
         \x20            open it in the default browser, drag to select, double-click\n\
         \x20            selects a word, triple-click selects the line, scroll to move,\n\
         \x20            single-click sidebar to select, double-click to open, scroll to browse\n\
         \x20 sidebar:  arrows/Enter open, Backspace goes up\n\
         \x20 editor:   type, arrows (+Shift to select), Home/End, PgUp/PgDn, Backspace, Delete, Tab\n\
         \n\
         images:  opening an image file (png/jpg/gif/webp/…) previews it in the\n\
         \x20            editor pane via the terminal's graphics protocol (kitty,\n\
         \x20            sixel, iTerm2, or unicode half-blocks as a last resort); Esc closes\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_cell_size_scale_one_is_unchanged() {
        assert_eq!(
            logical_cell_size_from_scale(FontSize::new(16, 34), Some(1.0))
                .map(|size| (size.width, size.height)),
            Some((16, 34))
        );
    }

    #[test]
    fn logical_cell_size_scale_two_matches_retina_dimensions() {
        assert_eq!(
            logical_cell_size_from_scale(FontSize::new(16, 34), Some(2.0))
                .map(|size| (size.width, size.height)),
            Some((8, 17))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn invalid_or_unavailable_macos_scale_uses_the_known_good_fallback() {
        for scale in [
            None,
            Some(0.0),
            Some(-1.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ] {
            let logical = macos_logical_cell_size_or_fallback(FontSize::new(16, 34), scale);
            assert_eq!((logical.width, logical.height), (8, 16));
        }
    }

    #[test]
    fn non_macos_logical_cell_size_uses_reported_dimensions_unchanged() {
        let logical = non_macos_logical_cell_size(FontSize::new(16, 34));
        assert_eq!((logical.width, logical.height), (16, 34));
    }

    #[test]
    fn resolve_start_missing_file_binds_the_path_without_creating_it() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join("ratatata-resolve-missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("somefile.md");
        assert!(!file.exists());

        let (resolved_dir, resolved_file) = resolve_start(Some(file.clone())).unwrap();
        let canon = std::fs::canonicalize(&dir).unwrap();
        assert_eq!(resolved_dir, canon);
        assert_eq!(resolved_file, Some(canon.join("somefile.md")));
        assert!(!file.exists());
    }

    #[test]
    fn help_text_keeps_images_block_aligned_with_other_sections() {
        let help = usage_text();
        assert!(help.contains("rat -V | --version"));
        assert!(help.contains("-V, --version  Print the version and exit"));
        assert!(help.contains("A path that does not exist yet opens an empty buffer"));
        assert!(help.contains("Ctrl+W   toggle word wrapping of long lines"));
        assert!(help.contains("Ctrl+H   show or hide dotfiles in the sidebar"));
        assert!(help.contains("Ctrl+B   show or hide the file sidebar"));
        assert!(help.contains("Ctrl+G   go to line"));
        assert!(help.contains("Ctrl+Shift+H  find and replace"));
        assert!(help.contains("Ctrl+A   select all"));
        let images = help
            .lines()
            .find(|line| line.trim_start().starts_with("images:"))
            .expect("images blurb");
        // the broken continuation used to dump the images header under the
        // keys list with a deep indent (more spaces than "keys" / "editor")
        let keys = help
            .lines()
            .find(|line| line.contains("keys (Cmd"))
            .unwrap();
        let keys_indent = keys.len() - keys.trim_start().len();
        let images_indent = images.len() - images.trim_start().len();
        assert_eq!(images_indent, keys_indent, "images={images:?}");
        assert!(
            !help.contains("sixel,            iTerm2"),
            "sixel line must not pick up source-indent spaces:\n{help}"
        );
        let sixel = help.lines().find(|line| line.contains("sixel")).unwrap();
        let mouse_cont = help
            .lines()
            .find(|line| line.contains("open it in the default browser"))
            .unwrap();
        let sixel_indent = sixel.len() - sixel.trim_start().len();
        let mouse_indent = mouse_cont.len() - mouse_cont.trim_start().len();
        assert_eq!(sixel_indent, mouse_indent, "sixel={sixel:?}");
    }
}
