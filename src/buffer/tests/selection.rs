use super::*;

#[test]
fn tab_indents_selected_lines() {
    let mut b = empty();
    b.insert_multiline("a\nb\nc");
    b.home();
    b.move_up();
    b.move_up(); // top-left
    b.begin_selection();
    b.move_down();
    b.end();
    b.indent();
    assert_eq!(b.lines, vec!["    a", "    b", "c"]);
    assert_eq!(b.selection_anchor, Some((0, 0)));
    assert_eq!(b.cursor, (5, 1));
}

#[test]
fn shift_tab_dedents_selected_lines() {
    let mut b = empty();
    b.insert_multiline("    a\n    b\n    c");
    b.home();
    b.move_up();
    b.move_up(); // top-left
    b.begin_selection();
    b.move_down();
    b.move_down();
    b.end();
    b.dedent();
    assert_eq!(b.lines, vec!["a", "b", "c"]);
    assert_eq!(b.selection_anchor, Some((0, 0)));
    assert_eq!(b.cursor, (1, 2));
}

#[test]
fn selection_requires_anchor_different_from_cursor() {
    let mut b = empty();
    typed(&mut b, "ab");
    b.home();
    assert!(!b.has_selection());
    b.begin_selection();
    assert!(!b.has_selection()); // anchor == cursor
    b.move_right();
    assert!(b.has_selection());
    assert_eq!(b.selection_range(), Some(((0, 0), (1, 0))));
    // moving backwards normalizes the range
    b.begin_selection();
    b.home();
    assert_eq!(b.selection_range(), Some(((0, 0), (1, 0))));
}

#[test]
fn selected_text_same_line_and_multi_line() {
    let mut b = empty();
    typed(&mut b, "one two");
    b.home();
    b.begin_selection();
    b.move_right();
    b.move_right();
    b.move_right();
    assert_eq!(b.selected_text().as_deref(), Some("one"));

    // multi-line selection: (0,4) .. (2,5)
    let mut b = empty();
    typed(&mut b, "one two");
    b.newline();
    typed(&mut b, "second");
    b.newline();
    typed(&mut b, "third");
    b.home();
    b.move_up();
    b.move_up();
    b.move_right();
    b.move_right();
    b.move_right();
    b.move_right();
    b.begin_selection();
    b.end();
    b.move_down();
    b.move_down();
    b.move_right();
    b.move_right();
    b.move_right();
    assert_eq!(b.selected_text().as_deref(), Some("two\nsecond\nthird"));
}

#[test]
fn selection_on_line_ranges() {
    let mut b = empty();
    typed(&mut b, "abc");
    b.newline();
    typed(&mut b, "def");
    b.newline();
    typed(&mut b, "ghi");
    b.home();
    b.move_up();
    b.move_up();
    b.move_down();
    b.move_right();
    b.begin_selection(); // anchor (1,1)
    b.move_down();
    b.move_right();
    b.move_right();
    assert_eq!(b.selection_on_line(0), None);
    assert_eq!(b.selection_on_line(1), Some((1, 3)));
    assert_eq!(b.selection_on_line(2), Some((0, 3)));
}

#[test]
fn delete_selection_same_line() {
    let mut b = empty();
    typed(&mut b, "hello world");
    b.home();
    b.move_right();
    b.move_right();
    b.begin_selection();
    b.move_right();
    b.move_right();
    b.move_right();
    b.move_right();
    b.move_right();
    b.delete_selection();
    assert_eq!(b.lines, vec!["heorld"]); // "llo w" was removed
    assert_eq!(b.cursor, (2, 0));
    assert!(!b.has_selection());
}

#[test]
fn delete_selection_multi_line() {
    let mut b = empty();
    typed(&mut b, "abc");
    b.newline();
    typed(&mut b, "def");
    b.newline();
    typed(&mut b, "ghi");
    // select from (1,0) to (3,2): "bc" + "def" + "ghi"
    b.home();
    b.move_up();
    b.move_up();
    b.move_right();
    b.begin_selection();
    b.move_down();
    b.move_down();
    b.move_right();
    b.move_right();
    b.delete_selection();
    assert_eq!(b.lines, vec!["a"]);
    assert_eq!(b.cursor, (1, 0));
}

#[test]
fn typing_replaces_selection() {
    let mut b = empty();
    typed(&mut b, "hello");
    b.home();
    b.move_right();
    b.begin_selection();
    b.move_right();
    b.move_right();
    b.insert_char('X');
    assert_eq!(b.lines, vec!["hXlo"]); // "el" was replaced
    assert!(!b.has_selection());
}

#[test]
fn backspace_and_delete_replace_selection() {
    let mut b = empty();
    typed(&mut b, "hello");
    b.home();
    b.begin_selection();
    b.end();
    b.backspace();
    assert_eq!(b.lines, vec![""]);

    typed(&mut b, "hello");
    b.home();
    b.begin_selection();
    b.end();
    b.delete();
    assert_eq!(b.lines, vec![""]);
}

#[test]
fn select_all_and_selected_text() {
    let mut b = empty();
    typed(&mut b, "one");
    b.newline();
    typed(&mut b, "two");
    b.select_all();
    assert_eq!(b.selected_text().as_deref(), Some("one\ntwo"));
    // select all + type replaces everything
    b.insert_char('z');
    assert_eq!(b.lines, vec!["z"]);
}

#[test]
fn select_word_at_selects_alphanumeric_run() {
    let mut b = empty();
    typed(&mut b, "foo_bar baz");
    b.select_word_at((4, 0)); // inside foo_bar
    assert_eq!(b.selected_text().as_deref(), Some("foo_bar"));
    // the next word
    b.select_word_at((8, 0)); // inside baz
    assert_eq!(b.selected_text().as_deref(), Some("baz"));
    // clicking past the end of the line selects the last word
    b.select_word_at((11, 0));
    assert_eq!(b.selected_text().as_deref(), Some("baz"));
}

#[test]
fn select_word_at_selects_punctuation_run() {
    let mut b = empty();
    typed(&mut b, "a -> b");
    b.select_word_at((3, 0)); // on '-'
    assert_eq!(b.selected_text().as_deref(), Some("->"));
    // punctuation runs stop at word chars: clicking '.' in a.b.c
    let mut b = empty();
    typed(&mut b, "a.b.c");
    b.select_word_at((1, 0));
    assert_eq!(b.selected_text().as_deref(), Some("."));
}

#[test]
fn select_word_at_selects_whitespace() {
    let mut b = empty();
    typed(&mut b, "hello   world");
    b.select_word_at((1, 0));
    assert_eq!(b.selected_text().as_deref(), Some("hello"));
    // clicking the whitespace between words selects its whole run
    b.select_word_at((6, 0));
    assert_eq!(b.selected_text().as_deref(), Some("   "));
    assert_eq!(b.selection_range(), Some(((5, 0), (8, 0))));
    // an empty line selects nothing
    let mut b = empty();
    b.newline();
    b.select_word_at((0, 1));
    assert!(!b.has_selection());
}

#[test]
fn select_word_at_handles_unicode() {
    let mut b = empty();
    typed(&mut b, "héllo wörld");
    b.select_word_at((3, 0)); // inside héllo
    assert_eq!(b.selected_text().as_deref(), Some("héllo"));
    b.select_word_at((7, 0)); // inside wörld
    assert_eq!(b.selected_text().as_deref(), Some("wörld"));
}

#[test]
fn select_line_selects_whole_line() {
    let mut b = empty();
    typed(&mut b, "one two");
    b.newline();
    typed(&mut b, "three");
    b.select_line(1);
    assert_eq!(b.selected_text().as_deref(), Some("three"));
    assert_eq!(b.selection_range(), Some(((0, 1), (5, 1))));
    // an empty line still yields an empty (non-)selection
    b.newline();
    b.select_line(2);
    assert!(!b.has_selection());
    assert_eq!(b.cursor, (0, 2));
}

#[test]
fn extend_selection_word_at_moves_cursor_to_word_end() {
    let mut b = empty();
    typed(&mut b, "one two three");
    b.select_word_at((0, 0)); // "one"
    // drag into "three" -> the selection ends at its word end
    b.extend_selection_word_at((8, 0));
    assert_eq!(b.selected_text().as_deref(), Some("one two three"));
    // dragging back into the first word shrinks it again
    b.extend_selection_word_at((2, 0));
    assert_eq!(b.selected_text().as_deref(), Some("one"));
    // dragging onto whitespace extends to the end of its run
    b.extend_selection_word_at((3, 0));
    assert_eq!(b.selected_text().as_deref(), Some("one "));
    assert_eq!(b.cursor, (4, 0));
}

#[test]
fn extend_selection_line_at_moves_cursor_to_line_end() {
    let mut b = empty();
    typed(&mut b, "one");
    b.newline();
    typed(&mut b, "two");
    b.newline();
    typed(&mut b, "three");
    b.select_line(0);
    b.extend_selection_line_at((0, 2));
    assert_eq!(b.selected_text().as_deref(), Some("one\ntwo\nthree"));
}
