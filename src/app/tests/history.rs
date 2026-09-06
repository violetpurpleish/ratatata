use super::*;

#[test]
fn clicking_undo_and_redo_buttons_work() {
    let dir = scratch("mbtnundo");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);

    app.handle_key(char_key('X'));
    assert_eq!(app.buffer.lines, vec!["Xalpha"]);
    click_button(&mut app, Shortcut::Undo);
    assert_eq!(app.buffer.lines, vec!["alpha"]);
    click_button(&mut app, Shortcut::Redo);
    assert_eq!(app.buffer.lines, vec!["Xalpha"]);
}

#[test]
fn ctrl_z_undoes_and_ctrl_shift_z_redoes() {
    let dir = scratch("undoredo1");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(file)).unwrap();

    app.handle_key(char_key('X'));
    assert_eq!(app.buffer.lines, vec!["Xalpha"]);
    assert!(app.buffer.dirty);

    app.handle_key(ctrl('z'));
    assert_eq!(app.buffer.lines, vec!["alpha"]);
    assert!(!app.buffer.dirty);
    assert_eq!(app.buffer.cursor, (0, 0));

    app.handle_key(ctrl_shift('z'));
    assert_eq!(app.buffer.lines, vec!["Xalpha"]);
    assert!(app.buffer.dirty);
    assert_eq!(app.buffer.cursor, (1, 0));
}

#[test]
fn cmd_z_and_cmd_shift_z_work_like_ctrl() {
    let dir = scratch("undoredo2");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(file)).unwrap();

    app.handle_key(char_key('X'));
    app.handle_key(cmd('z'));
    assert_eq!(app.buffer.lines, vec!["alpha"]);
    app.handle_key(cmd_shift('z'));
    assert_eq!(app.buffer.lines, vec!["Xalpha"]);
}

#[test]
fn undo_works_from_sidebar_focus() {
    let dir = scratch("undoredo4");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(file)).unwrap();

    app.handle_key(char_key('X'));
    app.handle_key(ctrl('o')); // switch to the sidebar
    assert_eq!(app.focus, Focus::Sidebar);
    app.handle_key(ctrl('z'));
    assert_eq!(app.buffer.lines, vec!["alpha"]);
}

#[test]
fn undo_rehighlights_restored_text() {
    let dir = scratch("undoredo7");
    let file = dir.join("code.rs");
    fs::write(&file, "fn main() {\n}\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    let buf = render_buffer(&mut app);
    assert_ne!(buf.cell((31, 2)).unwrap().style().fg, Some(Color::Reset));

    // typing breaks the keyword, undo restores both text and color
    app.handle_key(char_key('x'));
    let buf = render_buffer(&mut app);
    assert_eq!(buf.cell((31, 3)).unwrap().style().fg, Some(Color::Reset));

    app.handle_key(ctrl('z'));
    let buf = render_buffer(&mut app);
    assert_eq!(buf.cell((31, 3)).unwrap().symbol(), "f");
    assert_eq!(
        buf.cell((31, 3)).unwrap().style().fg,
        Some(Color::Rgb(203, 166, 247))
    );
}
