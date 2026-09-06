use super::*;

#[test]
fn editing_topbar_has_seventeen_shortcuts_with_quit_last() {
    let dir = scratch("topbar17");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    render_buffer(&mut app);
    let actions: Vec<Shortcut> = app.topbar_buttons.iter().map(|(a, _)| *a).collect();
    assert_eq!(actions.len(), 17);
    assert_eq!(*actions.last().unwrap(), Shortcut::Quit);
    for action in [
        Shortcut::SelectAll,
        Shortcut::ToggleSidebar,
        Shortcut::GoToLine,
        Shortcut::Replace,
        Shortcut::ToggleHidden,
    ] {
        assert!(actions.contains(&action), "missing {action:?}");
    }
    let rows = render(&mut app);
    assert!(row_contains(&rows, "Ctrl+A all"));
    assert!(row_contains(&rows, "Ctrl+B files"));
    assert!(row_contains(&rows, "Ctrl+G line"));
    assert!(row_contains(&rows, "Ctrl+Shift+H replace"));
    assert!(row_contains(&rows, "Ctrl+Q quit"));
    // 150 columns is enough for the full 17-button set on two rows
    assert_eq!(app.topbar_buttons.len(), 17);
}

#[test]
fn copy_cut_paste_roundtrip() {
    let dir = scratch("clip1");
    let mut app = with_fake_clipboard(new_app(dir, None).unwrap());
    app.handle_key(ctrl('o'));
    for c in "hello world".chars() {
        app.handle_key(char_key(c));
    }
    // select "world" with shift+arrows
    app.handle_key(key(KeyCode::Home));
    for _ in 0..6 {
        app.handle_key(key(KeyCode::Right));
    }
    for _ in 0..5 {
        app.handle_key(shift_key(KeyCode::Right));
    }
    app.handle_key(ctrl('x')); // cut
    assert_eq!(app.buffer.lines, vec!["hello "]);
    assert_eq!(app.clipboard.get_text().as_deref(), Some("world"));

    app.handle_key(key(KeyCode::End));
    app.handle_key(ctrl('v')); // paste
    assert_eq!(app.buffer.lines, vec!["hello world"]);
}

#[test]
fn cmd_shortcuts_work_like_ctrl() {
    let dir = scratch("clip3");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha").unwrap();
    let mut app = with_fake_clipboard(new_app(dir.clone(), Some(file)).unwrap());
    app.handle_key(char_key('X'));
    assert!(app.buffer.dirty);
    app.handle_key(cmd('s')); // Cmd+S saves
    assert!(!app.buffer.dirty);
    assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "Xalpha");

    app.handle_key(cmd('o')); // Cmd+O switches focus
    assert_eq!(app.focus, Focus::Sidebar);
    app.handle_key(cmd('o'));
    assert_eq!(app.focus, Focus::Editor);
    app.handle_key(cmd('n')); // Cmd+N starts a new untitled buffer
    assert_eq!(app.buffer.lines, vec![""]);
    assert!(app.buffer.path.is_none());
    app.handle_key(cmd('q')); // Cmd+Q quits
    assert!(app.should_quit);
}

#[test]
fn bracketed_paste_inserts_multiline() {
    let dir = scratch("clip4");
    let mut app = new_app(dir, None).unwrap();
    app.handle_key(ctrl('o'));
    app.handle_key(char_key('a'));
    app.paste_text("b\nc".to_string());
    assert_eq!(app.buffer.lines, vec!["ab", "c"]);
    assert!(app.buffer.dirty);
}

#[test]
fn top_bar_lists_undo_shortcut() {
    let dir = scratch("undoredo8");
    let mut app = new_app(dir, None).unwrap();
    let rows = render(&mut app);
    // the shortcut buttons live in the top bar; which row they occupy
    // depends on width and pill order
    assert!(row_contains(&rows, "Ctrl+Z undo"));
    assert!(row_contains(&rows, "Ctrl+Shift+Z redo"));
    assert!(row_contains(&rows, "Ctrl+Q quit"));
}

#[test]
fn clipboard_failure_preserves_cut_selection_and_reports_copy_failure() {
    let mut app = new_app(scratch("release-clipboard"), None).unwrap();
    app.clipboard = Box::new(FailingClipboard);
    app.buffer.insert_text("important");
    app.buffer.select_all();
    app.cut_selection();
    assert_eq!(app.buffer.content(), "important");
    assert!(app.buffer.has_selection());
    assert!(app.message.as_ref().unwrap().0.contains("clipboard failed"));
    app.copy_selection();
    assert!(app.message.as_ref().unwrap().0.contains("clipboard failed"));
}
