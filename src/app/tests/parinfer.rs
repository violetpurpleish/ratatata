use super::*;

#[test]
fn parinfer_runs_for_clojure_files_through_the_editor() {
    let dir = scratch("parinfer-app-clj");
    let file = dir.join("core.clj");
    fs::write(&file, "").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    app.handle_key(char_key('('));
    assert_eq!(app.buffer.lines, vec!["()"]);
    assert_eq!(app.buffer.cursor, (1, 0));
    app.handle_key(char_key('a'));
    assert_eq!(app.buffer.lines, vec!["(a)"]);
    assert_eq!(app.buffer.cursor, (2, 0));
    // one logical undo for the typing run plus the automatic parens
    app.handle_key(ctrl('z'));
    assert_eq!(app.buffer.lines, vec![""]);
    app.handle_key(KeyEvent::new(
        KeyCode::Char('z'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert_eq!(app.buffer.lines, vec!["(a)"]);
}

#[test]
fn parinfer_leaves_non_clojure_files_alone() {
    let dir = scratch("parinfer-app-txt");
    let file = dir.join("notes.txt");
    fs::write(&file, "").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    app.handle_key(char_key('('));
    assert_eq!(app.buffer.lines, vec!["("]);
    assert_eq!(app.buffer.cursor, (1, 0));
}

#[test]
fn parinfer_incomplete_string_keeps_typed_text() {
    let dir = scratch("parinfer-app-str");
    let file = dir.join("core.cljs");
    fs::write(&file, "").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    app.handle_key(char_key('"'));
    app.handle_key(char_key('h'));
    app.handle_key(char_key('i'));
    assert_eq!(app.buffer.lines, vec!["\"hi"]);
    assert_eq!(app.buffer.cursor, (3, 0));
}

#[test]
fn parinfer_goto_line_then_edit_uses_moved_cursor() {
    let mut app = parinfer_two_forms_app("parinfer-app-goto");
    assert_eq!(app.buffer.cursor, (0, 0));
    assert_eq!(app.buffer.parinfer_prev_cursor(), (0, 0));

    app.handle_key(ctrl('g'));
    app.handle_key(char_key('2'));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.buffer.cursor, (0, 1));
    assert_eq!(app.buffer.parinfer_prev_cursor(), (0, 1));
    assert_eq!(app.buffer.lines, vec!["(foo)", "(bar)"]);

    app.handle_key(char_key('x'));
    assert_eq!(app.buffer.lines, vec!["(foo)", "x(bar)"]);
    assert_eq!(app.buffer.cursor, (1, 1));
    assert_eq!(app.buffer.parinfer_prev_cursor(), (1, 1));
}

#[test]
fn parinfer_search_jump_then_edit_uses_moved_cursor() {
    let mut app = parinfer_two_forms_app("parinfer-app-search");
    open_search_typed(&mut app, "bar");
    assert_eq!(app.buffer.cursor, (1, 1));
    assert_eq!(app.buffer.parinfer_prev_cursor(), (1, 1));
    assert_eq!(app.buffer.lines, vec!["(foo)", "(bar)"]);

    app.handle_key(key(KeyCode::Esc));
    app.handle_key(char_key('x'));
    assert_eq!(app.buffer.lines, vec!["(foo)", "(xbar)"]);
    assert_eq!(app.buffer.cursor, (2, 1));
    assert_eq!(app.buffer.parinfer_prev_cursor(), (2, 1));
}

#[test]
fn parinfer_select_all_then_edit_uses_moved_cursor() {
    let mut app = parinfer_two_forms_app("parinfer-app-selectall");
    app.handle_key(ctrl('a'));
    assert_eq!(app.buffer.cursor, (5, 1));
    assert_eq!(app.buffer.parinfer_prev_cursor(), (5, 1));
    assert_eq!(app.buffer.lines, vec!["(foo)", "(bar)"]);

    app.handle_key(char_key('x'));
    assert_eq!(app.buffer.lines, vec!["x"]);
    assert_eq!(app.buffer.cursor, (1, 0));
    assert_eq!(app.buffer.parinfer_prev_cursor(), (1, 0));
}

#[test]
fn parinfer_reload_clamps_prev_cursor_before_next_edit() {
    let dir = scratch("parinfer-app-reload");
    let file = dir.join("core.clj");
    fs::write(&file, "(foo)\n(bar)\n(baz)\n(quux)").unwrap();
    let mut app = new_app(dir, Some(file.clone())).unwrap();
    render_buffer(&mut app);
    app.handle_key(key(KeyCode::End));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::End));
    assert_eq!(app.buffer.cursor, (6, 3));
    assert_eq!(app.buffer.parinfer_prev_cursor(), (6, 3));

    fs::write(&file, "(a)").unwrap();
    app.handle_key(ctrl('r'));
    assert_eq!(app.buffer.lines, vec!["(a)"]);
    assert_eq!(app.buffer.cursor, (3, 0));
    assert_eq!(app.buffer.parinfer_prev_cursor(), (3, 0));

    app.handle_key(char_key('x'));
    assert_eq!(app.buffer.lines, vec!["(a)x"]);
    assert_eq!(app.buffer.cursor, (4, 0));
    assert_eq!(app.buffer.parinfer_prev_cursor(), (4, 0));
}

#[test]
fn parinfer_keyboard_navigation_does_not_rewrite_buffer() {
    let dir = scratch("parinfer-app-nav");
    let file = dir.join("core.clj");
    fs::write(&file, "(foo\nbar").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    let before = app.buffer.lines.clone();
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.buffer.cursor.1, 1);
    assert_eq!(app.buffer.lines, before);
    for code in [
        KeyCode::Right,
        KeyCode::End,
        KeyCode::Home,
        KeyCode::Up,
        KeyCode::PageDown,
        KeyCode::PageUp,
        KeyCode::Left,
    ] {
        app.handle_key(key(code));
        assert_eq!(app.buffer.lines, before, "{code:?}");
    }
    assert!(!app.buffer.dirty);
    app.handle_key(ctrl('z'));
    assert_eq!(app.buffer.lines, before, "navigation must not create undo");

    // Typing after navigation still runs Smart Mode.
    app.handle_key(key(KeyCode::End));
    app.handle_key(char_key('x'));
    assert!(app.buffer.lines.join("\n").contains('x'));
    assert!(app.buffer.dirty);
}

#[test]
fn parinfer_mouse_cursor_placement_does_not_rewrite_buffer() {
    let dir = scratch("parinfer-app-mouse");
    let file = dir.join("core.clj");
    fs::write(&file, "(foo\nbar").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);
    let before = app.buffer.lines.clone();
    let inner_x = app.editor_area.x as usize + 1;
    let inner_y = app.editor_area.y as usize + 1;
    let gutter_w = app.buffer.lines.len().to_string().len() + 1;
    let text_x = (inner_x + gutter_w + 1) as u16;
    let line0_y = inner_y as u16;
    let line1_y = (inner_y + 1) as u16;

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        text_x,
        line0_y,
    ));
    assert_eq!(app.focus, Focus::Editor);
    assert_eq!(app.buffer.cursor.1, 0);
    assert!(app.buffer.cursor.0 > 0, "click should land in the text");
    assert_eq!(app.buffer.lines, before);

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        text_x,
        line1_y,
    ));
    assert_eq!(app.buffer.cursor.1, 1);
    assert_eq!(app.buffer.lines, before);

    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        text_x + 2,
        line1_y,
    ));
    assert_eq!(app.buffer.cursor.1, 1);
    assert_eq!(app.buffer.lines, before);
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        text_x + 2,
        line1_y,
    ));
    assert_eq!(app.buffer.lines, before);
    assert!(!app.buffer.dirty);
    app.handle_key(ctrl('z'));
    assert_eq!(
        app.buffer.lines, before,
        "mouse placement must not create undo"
    );
}
