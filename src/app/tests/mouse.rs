use super::*;

#[test]
fn link_detection_handles_urls_and_surrounding_punctuation() {
    let line = "See https://example.com/docs, and www.example.org/path.";
    let https_col = line.find("https://").unwrap();
    let www_col = line.find("www.").unwrap();
    assert_eq!(
        link_at(line, https_col + 10).as_deref(),
        Some("https://example.com/docs")
    );
    assert_eq!(
        link_at(line, www_col + 5).as_deref(),
        Some("https://www.example.org/path")
    );
    assert_eq!(link_at(line, 0), None);
}

#[test]
fn link_detection_keeps_balanced_url_parentheses() {
    let line = "[site](https://example.com/a_(b)).";
    let col = line.find("example").unwrap();
    assert_eq!(
        link_at(line, col).as_deref(),
        Some("https://example.com/a_(b)")
    );
}

#[test]
fn renders_long_content_with_scrolling() {
    let dir = scratch("render4");
    let file = dir.join("long.txt");
    let content: String = (0..200).map(|i| format!("line {i:03}\n")).collect();
    fs::write(&file, content).unwrap();
    let mut app = new_app(dir.clone(), Some(file)).unwrap();

    // cursor at the end of the buffer; drawing must scroll it into view
    // without panicking
    app.buffer.cursor = (200, 199);
    let rows = render(&mut app);
    assert!(row_contains(&rows, "line 199"));
    assert!(!row_contains(&rows, "line 000"));
}

#[test]
fn renders_editor_scrollbar_for_long_content() {
    let dir = scratch("render-scrollbar");
    let file = dir.join("long.txt");
    let content: String = (0..200).map(|i| format!("line {i:03}\n")).collect();
    fs::write(&file, content).unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    let buf = render_buffer(&mut app);
    let scrollbar_x = app.editor_area.x + app.editor_area.width - 1;
    let mut scrollbar_range = app.editor_area.y + 1..app.editor_area.y + app.editor_area.height - 1;
    let scrollbar = scrollbar_range
        .clone()
        .map(|y| buf.cell((scrollbar_x, y)).unwrap().symbol())
        .collect::<String>();
    assert!(scrollbar.contains('█'));
    assert!(scrollbar.contains('│'));
    let thumb_y = scrollbar_range
        .find(|&y| buf.cell((scrollbar_x, y)).unwrap().symbol() == "█")
        .unwrap();
    assert_eq!(
        buf.cell((scrollbar_x, thumb_y)).unwrap().style().fg,
        Some(FOCUS_COLOR)
    );

    // At the final viewport the thumb reaches the bottom of the track.
    app.buffer.scroll.1 = app
        .buffer
        .total_visual_rows()
        .saturating_sub(app.editor_text.1 as usize);
    let buf = render_buffer(&mut app);
    let bottom = app.editor_area.y + app.editor_area.height - 2;
    assert_eq!(buf.cell((scrollbar_x, bottom)).unwrap().symbol(), "█");
    assert_eq!(
        buf.cell((scrollbar_x, bottom)).unwrap().style().fg,
        Some(FOCUS_COLOR)
    );
}

#[test]
fn wrapped_mouse_click_maps_to_visual_rows() {
    let dir = scratch("wrap3");
    let file = dir.join("a.txt");
    let line = "x".repeat(250);
    fs::write(&file, format!("{line}\n")).unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    app.handle_key(ctrl('w'));
    render_buffer(&mut app); // sets viewport and wrap width

    // terminal col 36 = text col 5 on the second visual row (char 123)
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 36, 4));
    assert_eq!(app.buffer.cursor, (123, 0));

    // gutter of the third visual row lands at its start (char 236)
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 5));
    assert_eq!(app.buffer.cursor, (236, 0));
}

#[test]
fn mouse_click_positions_cursor_and_focuses_editor() {
    let dir = scratch("mclick");
    let file = dir.join("code.rs");
    fs::write(&file, "fn main() {\n    let x = 1;\n}\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app); // sets widget areas

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 35, 4));
    assert_eq!(app.focus, Focus::Editor);
    // terminal col 35 = text col 4 on line 2 (index 1)
    assert_eq!(app.buffer.cursor, (4, 1));

    // clicking in the gutter lands at column 0
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 29, 5));
    assert_eq!(app.buffer.cursor, (0, 2));
}

#[test]
fn mouse_drag_selects_text() {
    let dir = scratch("mdrag");
    let file = dir.join("code.rs");
    fs::write(&file, "fn main() {\n}\n").unwrap();
    let mut app = with_fake_clipboard(new_app(dir, Some(file)).unwrap());
    render_buffer(&mut app);

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 31, 2));
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 33, 2));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 33, 2));
    assert_eq!(app.buffer.selected_text().as_deref(), Some("fn"));

    // Ctrl+C copies the selection to the clipboard
    app.handle_key(ctrl('c'));
    assert_eq!(app.clipboard.get_text().as_deref(), Some("fn"));
}

#[test]
fn shift_click_extends_selection() {
    let dir = scratch("mshift");
    let file = dir.join("code.rs");
    fs::write(&file, "fn main() {\n}\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 33, 2));
    let shift_click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 36,
        row: 2,
        modifiers: KeyModifiers::SHIFT,
    };
    app.handle_mouse(shift_click);
    assert_eq!(app.buffer.selection_range(), Some(((2, 0), (5, 0))));
}

#[test]
fn double_click_selects_word() {
    let dir = scratch("mdblword");
    let file = dir.join("a.txt");
    fs::write(&file, "hello brave world\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);

    // terminal col 37 = char 6 = 'b' of "brave"
    let click = |kind| mouse(kind, 37, 2);
    // first click just places the cursor
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    assert!(!app.buffer.has_selection());
    assert_eq!(app.buffer.cursor, (6, 0));

    // second click on the same spot within the TTL selects the word
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    assert_eq!(app.buffer.selected_text().as_deref(), Some("brave"));
    // selecting is finished (mouse released)
    assert!(!app.buffer.selecting);
}

#[test]
fn triple_click_selects_line() {
    let dir = scratch("mtriple");
    let file = dir.join("a.txt");
    fs::write(&file, "one two three\nfour five\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);

    let click = |kind| mouse(kind, 31, 2); // char 0 of line 0
    for _ in 0..3 {
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    }
    assert_eq!(app.buffer.selected_text().as_deref(), Some("one two three"));
    assert_eq!(app.buffer.selection_range(), Some(((0, 0), (13, 0))));

    // a fourth rapid click starts a fresh sequence: it's a plain click
    // and clears the line selection (extra clicks never get stuck)
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    assert!(!app.buffer.has_selection());
}

#[test]
fn single_click_then_double_click_selects_word_not_line() {
    let dir = scratch("mdblafterclick");
    let file = dir.join("a.txt");
    fs::write(&file, "hello brave world\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);

    let click = |kind| mouse(kind, 37, 2); // char 6 = 'b' of "brave"
    // the user clicks once to place the cursor...
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    // ...hesitates longer than the editor click window...
    app.last_editor_click = Some((Instant::now() - Duration::from_millis(300), Some((6, 0)), 1));
    // ...then double-clicks the same word
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    // the double-click selects the word, not the whole line
    assert_eq!(app.buffer.selected_text().as_deref(), Some("brave"));
}

#[test]
fn drag_after_double_click_extends_by_word() {
    let dir = scratch("mdblworddrag");
    let file = dir.join("a.txt");
    fs::write(&file, "hello brave new world\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);

    let click = |kind| mouse(kind, 37, 2); // char 6 = 'b' of "brave"
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    // drag into "world" (char 16) -> extends word-wise
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 47, 2));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    assert_eq!(
        app.buffer.selected_text().as_deref(),
        Some("brave new world")
    );
}

#[test]
fn drag_jitter_inside_word_keeps_word_selection() {
    let dir = scratch("mdbljitter");
    let file = dir.join("a.txt");
    fs::write(&file, "hello brave world\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);

    let click = |kind| mouse(kind, 37, 2); // char 6 = 'b' of "brave"
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    // hand jitter while holding the second click: still inside "brave"
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 39, 2));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    assert_eq!(app.buffer.selected_text().as_deref(), Some("brave"));
}

#[test]
fn drag_after_triple_click_extends_by_line() {
    let dir = scratch("mtripledrag");
    let file = dir.join("a.txt");
    fs::write(&file, "one two three\nfour five\nsix\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);

    let click = |kind| mouse(kind, 31, 3); // char 0 of line 0
    for _ in 0..3 {
        app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
        app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    }
    // drag to line 2 (terminal row 5) -> extends line-wise
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 31, 5));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    assert_eq!(
        app.buffer.selected_text().as_deref(),
        Some("one two three\nfour five\nsix")
    );
}

#[test]
fn multi_click_count_resets_on_new_position() {
    let dir = scratch("mmulticlick");
    let file = dir.join("a.txt");
    fs::write(&file, "hello brave world\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);

    // double-click "brave" -> word selected
    let click = |kind| mouse(kind, 37, 2);
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    assert_eq!(app.buffer.selected_text().as_deref(), Some("brave"));

    // clicking a different position right away is a plain click again
    let other = mouse(MouseEventKind::Down(MouseButton::Left), 31, 2);
    app.handle_mouse(other);
    assert!(!app.buffer.has_selection());
    assert_eq!(app.buffer.cursor, (0, 0));
}

#[test]
fn double_click_on_whitespace_selects_whitespace() {
    let dir = scratch("mdblspace");
    let file = dir.join("a.txt");
    fs::write(&file, "hello world\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);

    // terminal col 36 = char 5 = the space between the words
    let click = |kind| mouse(kind, 36, 2);
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    assert_eq!(app.buffer.selected_text().as_deref(), Some(" "));
    assert_eq!(app.buffer.selection_range(), Some(((5, 0), (6, 0))));
}

#[test]
fn mouse_wheel_scrolls_editor_and_sidebar() {
    let dir = scratch("mwheel");
    let file = dir.join("long.txt");
    let content: String = (0..30).map(|i| format!("line {i}\n")).collect();
    fs::write(&file, content).unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);

    assert_eq!(app.buffer.cursor, (0, 0));
    assert_eq!(app.buffer.scroll.1, 0);
    app.handle_mouse(mouse(MouseEventKind::ScrollDown, 60, 10));
    assert_eq!(app.buffer.cursor, (0, 0));
    assert_eq!(app.buffer.scroll.1, 3);
    let rows = render(&mut app);
    assert!(row_contains(&rows, "line 3"));
    assert!(!row_contains(&rows, "line 0"));
    app.handle_mouse(mouse(MouseEventKind::ScrollUp, 60, 10));
    assert_eq!(app.buffer.cursor, (0, 0));
    assert_eq!(app.buffer.scroll.1, 0);

    // wheel over the sidebar moves the selection
    app.handle_mouse(mouse(MouseEventKind::ScrollDown, 5, 5));
    assert_eq!(app.sidebar.selected, 1);

    // Horizontal wheel events over the sidebar must not scroll the editor.
    // The editor is already at the left edge, so a rightward event makes
    // the regression visible without depending on the file contents.
    assert_eq!(app.buffer.scroll.0, 0);
    app.handle_mouse(mouse(MouseEventKind::ScrollRight, 5, 5));
    assert_eq!(app.buffer.scroll.0, 0);

    // The same event still scrolls when it is actually over the editor.
    app.handle_mouse(mouse(MouseEventKind::ScrollRight, 60, 10));
    assert_eq!(app.buffer.scroll.0, 3);
    app.handle_mouse(mouse(MouseEventKind::ScrollLeft, 60, 10));
    assert_eq!(app.buffer.scroll.0, 0);
}

#[test]
fn sidebar_single_click_selects_double_click_opens() {
    let dir = scratch("mdblclick");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    let mut app = new_app(dir, None).unwrap();
    render_buffer(&mut app);

    // single click on the file row (row 1: ".." is row 0)
    let click = |kind| mouse(kind, 5, 4);
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    assert_eq!(app.sidebar.selected, 1);
    assert_eq!(app.focus, Focus::Sidebar);
    assert!(app.buffer.path.is_none()); // not opened yet

    // second click within the double-click window opens the file
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    assert_eq!(app.focus, Focus::Editor);
    assert_eq!(app.buffer.lines, vec!["alpha"]);
}

#[test]
fn clicking_switch_button_toggles_focus() {
    let dir = scratch("mbtnswitch");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    render_buffer(&mut app);

    assert_eq!(app.focus, Focus::Editor);
    click_button(&mut app, Shortcut::SwitchFocus);
    assert_eq!(app.focus, Focus::Sidebar);
    click_button(&mut app, Shortcut::SwitchFocus);
    assert_eq!(app.focus, Focus::Editor);
}

#[test]
fn clicking_quit_button_requires_two_clicks_when_dirty() {
    let dir = scratch("mbtnquit");
    let mut app = new_app(dir, None).unwrap();
    app.handle_key(ctrl('o'));
    app.handle_key(char_key('x'));
    render_buffer(&mut app);

    click_button(&mut app, Shortcut::Quit);
    assert!(!app.should_quit); // armed, not quit
    click_button(&mut app, Shortcut::Quit);
    assert!(app.should_quit);
}

#[test]
fn clicking_quit_button_quits_when_clean() {
    let dir = scratch("mbtnquitclean");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    render_buffer(&mut app);

    click_button(&mut app, Shortcut::Quit);
    assert!(app.should_quit);
}

#[test]
fn clicking_wrap_button_toggles_wrapping() {
    let dir = scratch("mbtnwrapbtn");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    render_buffer(&mut app);
    assert!(!app.buffer.wrap);
    click_button(&mut app, Shortcut::ToggleWrap);
    assert!(app.buffer.wrap);
    assert!(app.message.is_some());
    click_button(&mut app, Shortcut::ToggleWrap);
    assert!(!app.buffer.wrap);
}

#[test]
fn clicking_select_all_button_selects_the_buffer() {
    let dir = scratch("mbtnall");
    let file = dir.join("a.txt");
    fs::write(&file, "hello\nworld").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);
    assert!(!app.buffer.has_selection());
    click_button(&mut app, Shortcut::SelectAll);
    assert_eq!(app.buffer.selected_text().as_deref(), Some("hello\nworld"));
}

#[test]
fn clicking_top_bar_gap_does_nothing() {
    let dir = scratch("mbtngap");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    render_buffer(&mut app);
    assert_eq!(app.focus, Focus::Editor);

    // after the last button on its row there is empty bar; clicking it
    // must not move focus, arm quit, or do anything else
    let last = app
        .topbar_buttons
        .iter()
        .max_by_key(|(_, r)| (r.y, r.x + r.width))
        .map(|(_, r)| *r)
        .unwrap();
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        last.x + last.width + 2,
        last.y,
    ));
    assert_eq!(app.focus, Focus::Editor);
    assert!(!app.should_quit);
    assert!(!app.buffer.dirty);
}

#[test]
fn mouse_leaving_terminal_clears_hover_highlight() {
    let dir = scratch("mbtnleave");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    render_buffer(&mut app);

    // hover the first button...
    app.handle_mouse(mouse(MouseEventKind::Moved, 2, 0));
    let buf = render_buffer(&mut app);
    assert_eq!(
        buf.cell((1, 0)).unwrap().style().bg,
        Some(TOPBAR_PILL_BG_HOVER)
    );

    // ...then the terminal reports (0,0) when the mouse leaves the
    // window: the highlight (and the status-bar description) must go
    // away instead of sticking on the first button
    app.handle_mouse(mouse(MouseEventKind::Moved, 0, 0));
    let buf = render_buffer(&mut app);
    assert_eq!(buf.cell((1, 0)).unwrap().style().bg, Some(PALETTE.bg));
    let rows = render(&mut app);
    assert!(rows[23].contains("1:1"));
}
