use super::*;

#[test]
fn clip_ops_clips_to_display_columns_not_char_count() {
    let line = "X".repeat(40);
    let spans = clip_ops(
        &line,
        &[(None, 0..line.len())],
        0,
        8,
        None,
        &[],
        ColorSupport::TrueColor,
    );
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "XXXXXXXX");

    let line = "\tABCD";
    let spans = clip_ops(
        line,
        &[(None, 0..line.len())],
        0,
        6,
        None,
        &[],
        ColorSupport::TrueColor,
    );
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "    AB");
}

#[test]
fn renders_sidebar_editor_and_status() {
    let dir = scratch("render1");
    fs::create_dir(dir.join("docs")).unwrap();
    fs::write(dir.join("notes.txt"), "hello world").unwrap();
    let mut app = new_app(dir.clone(), None).unwrap();

    let rows = render(&mut app);
    // sidebar: dirs and files listed, selected row marked
    assert!(row_contains(&rows, "docs/"));
    assert!(row_contains(&rows, "notes.txt"));
    assert!(row_contains(&rows, "▶"));
    // editor: untitled block, empty buffer with line number 1
    assert!(row_contains(&rows, "untitled"));
    assert!(row_contains(&rows, "1 "));
    // top bar: the clickable shortcut buttons
    assert!(row_contains(&rows, "Ctrl+S save"));
    assert!(row_contains(&rows, "Ctrl+Shift+Z redo"));
    assert!(row_contains(&rows, "Ctrl+Q quit"));
    // status bar: focus tag + position; the shortcuts moved to the top
    assert!(row_contains(&rows, "SIDEBAR"));
    assert!(row_contains(&rows, "○ saved"));
    assert!(row_contains(&rows, "1:1"));
    assert!(!rows[23].contains("Ctrl+S"));
}

#[test]
fn renders_syntax_highlighted_code() {
    let dir = scratch("hlrender");
    let file = dir.join("code.rs");
    fs::write(&file, "fn main() {\n    let msg = \"hi\";\n}\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    let buf = render_buffer(&mut app);
    // "fn" keyword: mauve; "main" function name: blue
    // (colors probed from the Catppuccin Mocha theme)
    assert_eq!(buf.cell((31, 3)).unwrap().symbol(), "f");
    assert_eq!(
        buf.cell((31, 3)).unwrap().style().fg,
        Some(Color::Rgb(203, 166, 247))
    );
    assert_eq!(buf.cell((34, 3)).unwrap().symbol(), "m");
    assert_eq!(
        buf.cell((34, 3)).unwrap().style().fg,
        Some(Color::Rgb(137, 180, 250))
    );
    // string content "hi": green
    assert_eq!(buf.cell((46, 4)).unwrap().symbol(), "h");
    assert_eq!(
        buf.cell((46, 4)).unwrap().style().fg,
        Some(Color::Rgb(166, 227, 161))
    );
    // punctuation uses Catppuccin's muted overlay color
    assert_eq!(buf.cell((31, 5)).unwrap().symbol(), "}");
    assert_eq!(
        buf.cell((31, 5)).unwrap().style().fg,
        Some(Color::Rgb(147, 153, 178))
    );
}

#[test]
fn caret_has_distinct_rendered_style() {
    let dir = scratch("caretstyle");
    let file = dir.join("notes.txt");
    fs::write(&file, "test\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    let buf = render_buffer(&mut app);
    let caret = buf.cell((31, 3)).unwrap();
    assert_eq!(caret.symbol(), "t");
    assert_eq!(caret.style().bg, Some(PALETTE.warning));
    assert!(!caret.style().add_modifier.contains(Modifier::REVERSED));
}

#[test]
fn ctrl_w_toggles_wrap() {
    let dir = scratch("wrap1");
    let file = dir.join("a.txt");
    fs::write(&file, "some content\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    assert!(!app.buffer.wrap);

    app.handle_key(ctrl('w'));
    assert!(app.buffer.wrap);
    assert!(app.message.is_some());

    app.handle_key(ctrl('w'));
    assert!(!app.buffer.wrap);
}

#[test]
fn wrapped_long_line_renders_across_rows() {
    let dir = scratch("wrap2");
    let file = dir.join("a.txt");
    // 150-col test terminal: 28 sidebar + 2 borders -> 118 text cols
    let line = "x".repeat(250);
    fs::write(&file, format!("{line}\n")).unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    app.handle_key(ctrl('w'));
    app.message = None; // let the wrap indicator show
    let rows = render(&mut app);

    // the line wraps onto three visual rows: the line number only on
    // the first, a blank gutter on the continuations (cols 29..31 are
    // the editor gutter; border chars are multi-byte, so slice chars)
    let gutter = |row: &str| -> String { row.chars().skip(29).take(2).collect() };
    assert_eq!(gutter(&rows[3]), "1 ");
    assert_eq!(gutter(&rows[4]), "  ");
    assert_eq!(gutter(&rows[5]), "  ");
    assert!(row_contains(&rows, &"x".repeat(118)));
    // the status bar shows the persistent wrap indicator
    assert!(rows[23].contains("wrap ○"));

    // back to no wrapping: one line per row, no indicator
    app.handle_key(ctrl('w'));
    app.message = None;
    let rows = render(&mut app);
    assert_eq!(gutter(&rows[3]), "1 ");
    assert_eq!(gutter(&rows[4]), "2 ");
    assert!(!rows[23].contains("wrap ○"));
}

#[test]
fn wrapped_caret_renders_on_the_cursor_row() {
    let dir = scratch("wrap4");
    let file = dir.join("a.txt");
    let line = "x".repeat(250);
    fs::write(&file, format!("{line}\n")).unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    app.handle_key(ctrl('w'));
    render_buffer(&mut app);

    // cursor on the first visual row: char 110 is col 110 of it
    app.buffer.cursor = (110, 0);
    let buf = render_buffer(&mut app);
    let cell = buf.cell((141, 3)).unwrap();
    assert_eq!(cell.symbol(), "x");
    assert_eq!(cell.style().bg, Some(PALETTE.warning));
}

#[test]
fn wrapped_text_breaks_at_words_in_rendering() {
    let dir = scratch("wrap5");
    let file = dir.join("a.txt");
    // 30 words of 6 chars (179 chars); at 118 text cols a row
    // holds 19 words and wraps after a space
    let line = "hello ".repeat(30).trim_end().to_string();
    fs::write(&file, format!("{line}\n")).unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    app.handle_key(ctrl('w'));
    app.message = None;
    let rows = render(&mut app);
    let text = |row: &str| -> String { row.chars().skip(31).take(118).collect() };
    let gutter = |row: &str| -> String { row.chars().skip(29).take(2).collect() };

    // first row ends exactly at the wrap point (after a space); the
    // rest of the 118-cell row is blank
    assert_eq!(
        text(&rows[3]).trim_end(),
        format!("{}hello", "hello ".repeat(18))
    );
    // second row continues with whole words and a blank gutter
    assert_eq!(gutter(&rows[4]), "  ");
    assert_eq!(
        text(&rows[4]).trim_end(),
        format!("{}hello", "hello ".repeat(10))
    );
}

#[test]
fn hovering_top_bar_button_describes_it_in_the_status_bar() {
    let dir = scratch("mbtnhover");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    render_buffer(&mut app);
    let rect = topbar_button(&app, Shortcut::Save);

    // moving the mouse over the button shows its description...
    app.handle_mouse(mouse(MouseEventKind::Moved, rect.x + 1, rect.y));
    let rows = render(&mut app);
    assert!(rows[23].contains("save the current file"));

    // ...and moving away restores the cursor position
    app.handle_mouse(mouse(MouseEventKind::Moved, 139, 10));
    let rows = render(&mut app);
    assert!(rows[23].contains("1:1"));
}

#[test]
fn hovering_top_bar_button_highlights_it() {
    let dir = scratch("mbtnhoverbg");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    let buf = render_buffer(&mut app);
    let hovered = topbar_button(&app, Shortcut::Save);
    let idle = app
        .topbar_buttons
        .iter()
        .find(|(a, _)| *a != Shortcut::Save)
        .map(|(_, r)| *r)
        .expect("another shortcut button to compare against");
    assert!(hovered.width > 0);
    assert!(hovered.x >= TOPBAR_INDENT);

    // the bar starts one cell in from the window edge: cell (0,0) is
    // plain margin, not part of the first button
    assert_eq!(buf.cell((0, 0)).unwrap().style().bg, Some(PALETTE.bg));

    // moving the mouse over the button highlights that button's own
    // rectangle, including when Save has wrapped onto a later row
    app.handle_mouse(mouse(MouseEventKind::Moved, hovered.x + 1, hovered.y));
    let buf = render_buffer(&mut app);
    for x in hovered.x..hovered.x + hovered.width {
        assert_eq!(
            buf.cell((x, hovered.y)).unwrap().style().bg,
            Some(TOPBAR_PILL_BG_HOVER),
            "col {x} row {}",
            hovered.y
        );
    }
    // ...while a non-hovered button blends into the base background
    assert_eq!(
        buf.cell((idle.x, idle.y)).unwrap().style().bg,
        Some(PALETTE.bg)
    );

    // moving away restores the base background
    app.handle_mouse(mouse(MouseEventKind::Moved, 140, 10));
    let buf = render_buffer(&mut app);
    assert_eq!(
        buf.cell((hovered.x + 1, hovered.y)).unwrap().style().bg,
        Some(PALETTE.bg)
    );
}

#[test]
fn top_bar_wraps_onto_a_second_row_on_narrow_terminals() {
    let dir = scratch("mbtnwrap");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();

    // 80 columns: the buttons flow onto extra rows but stay visible.
    // Which labels land on which row depends on pill order and width,
    // so this checks wrapping and hit-testing rather than a snapshot.
    let rows = render_rows(&mut app, 80, 24);
    assert!(
        app.topbar_buttons.iter().any(|(_, r)| r.y > 0),
        "expected wrapping at 80 columns, buttons={:?}",
        app.topbar_buttons
    );
    assert!(app.topbar_area.height >= 2);
    assert!(row_contains(&rows, "Ctrl+O switch"));
    // each hit rectangle matches the row that actually painted that label
    for (action, rect) in &app.topbar_buttons {
        let label = format!("{} {}", action.key_label(), action.action_label());
        assert!(
            rows[rect.y as usize].contains(&label),
            "{label} missing from row {}",
            rect.y
        );
    }
    // Quit is pinned, so wrapping keeps it visible instead of dropping
    // it behind the ellipsis.
    assert_eq!(app.topbar_buttons.last().unwrap().0, Shortcut::Quit);
    assert!(row_contains(&rows, "Ctrl+Q quit"));

    // clicking a button on a wrapped row still works
    let wrapped = app
        .topbar_buttons
        .iter()
        .find(|(a, r)| r.y > 0 && *a != Shortcut::Quit)
        .map(|(a, _)| *a)
        .expect("a non-quit button should wrap onto a later row");
    click_button(&mut app, wrapped);
}

#[test]
fn top_bar_keeps_quit_visible_at_1280px_air_width() {
    // Ghostty on a 13" Air at 1280 logical px is ~120 columns with a
    // ~10–11px cell. Two rows used to end `Ctrl+H hidden…` and omit Quit.
    let dir = scratch("mbtnair");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();

    let rows = render_rows(&mut app, 120, 24);
    let actions: Vec<Shortcut> = app.topbar_buttons.iter().map(|(a, _)| *a).collect();
    assert_eq!(actions.len(), 17, "{actions:?}");
    assert_eq!(*actions.last().unwrap(), Shortcut::Quit);
    assert!(row_contains(&rows, "Ctrl+Q quit"));
    assert!(row_contains(&rows, "Ctrl+H hidden"));
    assert!(row_contains(&rows, "Ctrl+Shift+H replace"));
    let quit = topbar_button(&app, Shortcut::Quit);
    assert!(
        quit.y > 0,
        "Quit should wrap off the first row at 120 columns, was y={}",
        quit.y
    );
    assert!(app.topbar_area.height >= 2);
    click_button(&mut app, Shortcut::Quit);
    assert!(app.should_quit);
}

#[test]
fn wrap_topbar_pills_stays_in_bounds_across_widths() {
    let dir = scratch("mbtnwrapbounds");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    let app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    let pills = app.shortcut_pills();

    for width in 40..=160 {
        let layout = wrap_topbar_pills(&pills, width);
        assert_eq!(
            layout.pills.last().map(|p| p.action),
            Some(Shortcut::Quit),
            "width {width}"
        );
        let mut prev: Option<(u16, u16, u16)> = None; // row, x, x+width
        for pill in &layout.pills {
            assert!(
                pill.x >= TOPBAR_INDENT,
                "width {width} {pill:?} starts before indent"
            );
            if width >= pill.width + TOPBAR_INDENT {
                assert!(
                    pill.x + pill.width <= width,
                    "width {width} {pill:?} overflows"
                );
            }
            if let Some((row, x, end)) = prev
                && row == pill.row
            {
                assert!(
                    pill.x >= end,
                    "width {width} overlap: prior ends {end} at x={x}, {pill:?}"
                );
            }
            prev = Some((pill.row, pill.x, pill.x + pill.width));
        }
    }

    let wide = wrap_topbar_pills(&pills, 150);
    assert_eq!(wide.pills.len(), pills.len());
    let narrow = wrap_topbar_pills(&pills, 80);
    assert!(
        narrow.pills.iter().any(|p| p.row > 0),
        "80 columns should wrap"
    );
}

#[test]
fn selection_renders_with_theme_background() {
    let dir = scratch("mselrender");
    let file = dir.join("a.txt");
    fs::write(&file, "hello world\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    app.buffer.home();
    app.buffer.begin_selection();
    app.buffer.end();
    let buf = render_buffer(&mut app);
    // cells inside the selection use the Catppuccin selection color
    for x in 31..42 {
        assert_eq!(
            buf.cell((x, 3)).unwrap().style().bg,
            Some(PALETTE.selection),
            "col {x}"
        );
    }
    // cells outside retain the themed base background
    assert_eq!(buf.cell((43, 3)).unwrap().style().bg, Some(PALETTE.bg));
}

#[test]
fn unwrapped_long_line_does_not_paint_over_right_border() {
    let dir = scratch("longline-border");
    let file = dir.join("wide.txt");
    fs::write(&file, format!("{}\n", "X".repeat(500))).unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    assert!(!app.buffer.wrap);
    let buf = render_buffer(&mut app);
    let border_x = app.editor_area.x + app.editor_area.width - 1;
    let y = app.editor_area.y + 1;
    let cell = buf.cell((border_x, y)).unwrap();
    assert_ne!(
        cell.symbol(),
        "X",
        "unwrapped text painted over the right border"
    );
    assert!(
        matches!(cell.symbol(), "│" | "┃" | "█" | "┤" | "┐" | "┘"),
        "expected a border or scrollbar glyph, got {:?}",
        cell.symbol()
    );

    // tabs expand to several columns; they must clip too
    let dir = scratch("longline-tabs");
    let file = dir.join("tabs.txt");
    fs::write(&file, format!("{}\n", "\t".repeat(80))).unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    let buf = render_buffer(&mut app);
    let border_x = app.editor_area.x + app.editor_area.width - 1;
    let y = app.editor_area.y + 1;
    let cell = buf.cell((border_x, y)).unwrap();
    assert_ne!(
        cell.symbol(),
        " ",
        "tab expansion overwrote the right border"
    );
    assert!(matches!(cell.symbol(), "│" | "┃" | "█" | "┤" | "┐" | "┘"));
}

#[test]
fn ansi16_fallback_uses_named_colors_instead_of_rgb() {
    let dir = scratch("ansi16-ui");
    let file = dir.join("notes.txt");
    fs::write(&file, "hello\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    app.set_color_support(ColorSupport::Ansi16);
    let buf = render_buffer(&mut app);
    let border = buf
        .cell((app.editor_area.x, app.editor_area.y + 1))
        .unwrap();
    assert_eq!(border.style().fg, Some(Color::LightBlue));
    assert_eq!(buf.cell((0, 0)).unwrap().style().bg, Some(Color::Black));
    // truecolor default is unchanged
    let dir = scratch("truecolor-ui");
    let file = dir.join("notes.txt");
    fs::write(&file, "hello\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    let buf = render_buffer(&mut app);
    let border = buf
        .cell((app.editor_area.x, app.editor_area.y + 1))
        .unwrap();
    assert_eq!(border.style().fg, Some(FOCUS_COLOR));
    assert_eq!(buf.cell((0, 0)).unwrap().style().bg, Some(PALETTE.bg));
}
