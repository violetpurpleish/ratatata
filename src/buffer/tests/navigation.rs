use super::*;

#[test]
fn newline_splits_line_at_cursor() {
    let mut b = empty();
    b.insert_text("abcd");
    b.move_left();
    b.move_left();
    b.newline();
    assert_eq!(b.lines, vec!["ab", "cd"]);
    assert_eq!(b.cursor, (0, 1));
}

#[test]
fn enter_at_end_creates_empty_line() {
    let mut b = empty();
    b.insert_text("ab");
    b.newline();
    assert_eq!(b.lines, vec!["ab", ""]);
    assert_eq!(b.cursor, (0, 1));
}

#[test]
fn dedent_removes_partial_indent() {
    let mut b = empty();
    typed(&mut b, "  foo");
    b.dedent();
    assert_eq!(b.lines, vec!["foo"]);
    assert_eq!(b.cursor, (3, 0));
}

#[test]
fn delete_merges_lines_at_end() {
    let mut b = empty();
    b.insert_text("one");
    b.newline();
    b.insert_text("two");
    b.move_up();
    b.end();
    b.delete();
    assert_eq!(b.lines, vec!["onetwo"]);
    assert_eq!(b.cursor, (3, 0));
}

#[test]
fn left_at_line_start_jumps_to_previous_line_end() {
    let mut b = empty();
    b.insert_text("ab");
    b.newline();
    b.move_left();
    assert_eq!(b.cursor, (2, 0));
}

#[test]
fn right_at_line_end_jumps_to_next_line() {
    let mut b = empty();
    b.insert_text("ab");
    b.newline();
    b.move_up();
    b.move_right();
    b.move_right();
    b.move_right();
    assert_eq!(b.cursor, (0, 1));
}

#[test]
fn wide_char_cursor_column() {
    let mut b = empty();
    b.insert_text("日本語");
    b.end();
    // 3 wide chars -> column 6
    assert_eq!(b.cursor_col(), 6);
    // scroll so the first char is off-screen
    b.scroll.0 = 2;
    assert_eq!(b.cursor_col(), 2);
}

#[test]
fn ensure_visible_scrolls_down_and_right() {
    let mut b = empty();
    // 20 lines total (0..=19); cursor ends on line 19
    for _ in 0..19 {
        b.newline();
        b.insert_text("x");
    }
    // make the last line long enough that x = 50 is valid
    b.end();
    for _ in 0..49 {
        b.insert_char('y');
    }
    b.cursor = (50, 19);
    b.ensure_visible(10, 30);
    assert_eq!(b.scroll.1, 10);
    assert_eq!(b.scroll.0, 21);
    // and back up
    b.cursor = (1, 3);
    b.ensure_visible(10, 30);
    assert_eq!(b.scroll.1, 3);
    assert_eq!(b.scroll.0, 1);
}

#[test]
fn vertical_scroll_is_independent_of_cursor() {
    let mut b = empty();
    b.lines = (0..20).map(|i| format!("line {i}")).collect();
    b.cursor = (0, 0);

    b.scroll_vertical(3, 5);
    assert_eq!(b.cursor, (0, 0));
    assert_eq!(b.scroll.1, 3);

    b.scroll_vertical(100, 5);
    assert_eq!(b.cursor, (0, 0));
    assert_eq!(b.scroll.1, 15);

    b.scroll_vertical(-4, 5);
    assert_eq!(b.cursor, (0, 0));
    assert_eq!(b.scroll.1, 11);
}

#[test]
fn page_up_down_clamp() {
    let mut b = empty();
    for _ in 0..10 {
        b.newline();
    }
    // 11 lines: 0..=10
    b.cursor = (0, 10);
    b.page_down(5);
    assert_eq!(b.cursor.1, 10);
    b.page_up(5);
    assert_eq!(b.cursor.1, 5);
    b.page_up(100);
    assert_eq!(b.cursor.1, 0);
}

#[test]
fn cursor_movement_splits_typing_runs() {
    let mut b = empty();
    b.insert_text("ab");
    b.move_left();
    b.insert_char('X');
    assert_eq!(b.lines, vec!["aXb"]);
    // three separate undo steps: "a", "b", then "X"
    b.undo();
    assert_eq!(b.lines, vec!["ab"]);
    assert_eq!(b.cursor, (1, 0)); // where the undone edit started
    b.undo();
    assert_eq!(b.lines, vec![""]);
    b.redo();
    b.redo();
    assert_eq!(b.lines, vec!["aXb"]);
}
