use super::*;

#[test]
fn tab_and_shift_tab_selection_undo_redo() {
    let mut b = empty();
    b.insert_multiline("a\nb");
    b.home();
    b.move_up(); // top-left
    b.begin_selection();
    b.move_down();
    b.indent();
    assert_eq!(b.lines, vec!["    a", "    b"]);
    b.undo();
    assert_eq!(b.lines, vec!["a", "b"]);
    assert_eq!(b.selection_anchor, Some((0, 0))); // selection restored
    b.redo();
    assert_eq!(b.lines, vec!["    a", "    b"]);
    b.dedent();
    assert_eq!(b.lines, vec!["a", "b"]);
    b.undo();
    assert_eq!(b.lines, vec!["    a", "    b"]);
}

#[test]
fn consecutive_dedents_merge_into_one_undo_step() {
    let mut b = empty();
    typed(&mut b, "        foo");
    b.dedent();
    b.dedent();
    assert_eq!(b.lines, vec!["foo"]);
    b.undo();
    assert_eq!(b.lines, vec!["        foo"]);
    assert_eq!(b.cursor, (11, 0));
}

#[test]
fn replace_line_range_swaps_a_span_and_undoes_as_one_step() {
    let mut b = empty();
    typed(&mut b, "foo bar foo");
    b.replace_line_range(0, 0, 3, "baz");
    assert_eq!(b.lines, vec!["baz bar foo"]);
    assert_eq!(b.cursor, (3, 0));
    b.replace_line_range(0, 8, 11, "qux");
    assert_eq!(b.lines, vec!["baz bar qux"]);
    assert!(b.undo());
    assert_eq!(b.lines, vec!["baz bar foo"]);
    assert!(b.undo());
    assert_eq!(b.lines, vec!["foo bar foo"]);
}

#[test]
fn replace_line_ranges_replaces_all_and_undoes_as_one_step() {
    let mut b = empty();
    typed(&mut b, "foo bar foo");
    b.newline();
    typed(&mut b, "foo");
    b.replace_line_ranges(&[(0, 0, 3), (0, 8, 11), (1, 0, 3)], "x");
    assert_eq!(b.lines, vec!["x bar x", "x"]);
    assert!(b.undo());
    assert_eq!(b.lines, vec!["foo bar foo", "foo"]);
    // empty replacement deletes each span
    b.replace_line_ranges(&[(0, 0, 3), (0, 8, 11)], "");
    assert_eq!(b.lines, vec![" bar ", "foo"]);
}

#[test]
fn continuous_typing_is_one_undo_step() {
    let mut b = empty();
    b.insert_text("hello");
    b.undo();
    assert_eq!(b.lines, vec![""]);
    b.redo();
    assert_eq!(b.lines, vec!["hello"]);
}

#[test]
fn backspace_run_is_one_undo_step() {
    let mut b = empty();
    b.insert_text("hello");
    b.backspace();
    b.backspace();
    assert_eq!(b.lines, vec!["hel"]);
    b.undo();
    assert_eq!(b.lines, vec!["hello"]);
    assert_eq!(b.cursor, (5, 0));
    b.undo();
    assert_eq!(b.lines, vec![""]);
}

#[test]
fn delete_run_is_one_undo_step() {
    let mut b = empty();
    b.insert_text("abc");
    b.home();
    b.delete();
    b.delete();
    assert_eq!(b.lines, vec!["c"]);
    b.undo();
    assert_eq!(b.lines, vec!["abc"]);
    assert_eq!(b.cursor, (0, 0));
}

#[test]
fn paste_is_single_undo_step() {
    let mut b = empty();
    b.insert_text("ab");
    b.newline();
    b.insert_text("cd");
    b.home();
    b.move_down();
    b.insert_multiline("X\nY\nZ");
    assert_eq!(b.lines, vec!["ab", "X", "Y", "Zcd"]);
    b.undo();
    assert_eq!(b.lines, vec!["ab", "cd"]);
    assert_eq!(b.cursor, (0, 1));
}

#[test]
fn typing_over_selection_is_single_undo_step() {
    let mut b = empty();
    b.insert_text("hello");
    b.home();
    b.begin_selection();
    b.move_right();
    b.move_right();
    b.insert_char('X');
    assert_eq!(b.lines, vec!["Xllo"]);
    b.undo();
    assert_eq!(b.lines, vec!["hello"]);
    // the replaced selection comes back along with the text
    assert_eq!(b.selection_anchor, Some((0, 0)));
    assert_eq!(b.cursor, (2, 0));
}

#[test]
fn paste_over_selection_is_single_undo_step() {
    let mut b = empty();
    b.insert_text("abc");
    b.home();
    b.begin_selection();
    b.move_right();
    b.insert_multiline("XY\nZ");
    assert_eq!(b.lines, vec!["XY", "Zbc"]);
    b.undo();
    assert_eq!(b.lines, vec!["abc"]);
    assert_eq!(b.selection_anchor, Some((0, 0)));
    assert_eq!(b.cursor, (1, 0));
}

#[test]
fn cut_undo_restores_selection() {
    let mut b = empty();
    b.insert_text("hello");
    b.home();
    b.begin_selection();
    b.end();
    b.delete_selection();
    assert_eq!(b.lines, vec![""]);
    assert!(!b.has_selection());
    b.undo();
    assert_eq!(b.lines, vec!["hello"]);
    assert_eq!(b.selection_anchor, Some((0, 0)));
    assert_eq!(b.cursor, (5, 0));
}

#[test]
fn new_edit_after_undo_clears_redo() {
    let mut b = empty();
    b.insert_text("a");
    b.undo();
    b.insert_text("b");
    assert!(!b.redo());
    assert_eq!(b.lines, vec!["b"]);
    b.undo();
    assert_eq!(b.lines, vec![""]);
}

#[test]
fn undo_and_redo_are_noops_on_empty_history() {
    let mut b = empty();
    assert!(!b.undo());
    assert!(!b.redo());
    b.insert_text("x");
    b.undo();
    assert!(!b.undo()); // already at the beginning
    b.redo();
    assert!(!b.redo()); // already at the end
}

#[test]
fn undo_history_is_capped() {
    let mut b = empty();
    for _ in 0..MAX_UNDO + 50 {
        b.move_right(); // break the typing chain each time
        b.insert_char('x');
    }
    assert_eq!(b.undo_stack.len(), MAX_UNDO);
    // undoing everything stops at the oldest kept snapshot: the
    // dropped entries' edits stay applied
    while b.undo() {}
    assert_eq!(b.lines, vec!["x".repeat(50)]);
}

#[test]
fn undo_sets_last_edit_line_for_rehighlighting() {
    let mut b = empty();
    b.insert_text("abc");
    b.newline();
    b.insert_text("def");
    b.home();
    b.move_down();
    b.insert_char('X'); // line 1 changed
    b.undo();
    assert_eq!(b.last_edit_line, Some(1));
    b.redo();
    assert_eq!(b.last_edit_line, Some(1));
}
