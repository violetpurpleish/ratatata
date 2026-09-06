use super::*;

#[test]
fn empty_buffer_has_one_line() {
    let b = empty();
    assert_eq!(b.lines, vec![""]);
    assert!(!b.dirty);
}

#[test]
fn newline_inherits_indentation() {
    let mut b = empty();
    b.insert_text("    fn foo() {");
    b.newline();
    assert_eq!(b.lines, vec!["    fn foo() {", "    "]);
    assert_eq!(b.cursor, (4, 1));
}

#[test]
fn newline_mid_indentation_keeps_full_indent() {
    let mut b = empty();
    typed(&mut b, "    foo");
    b.home();
    b.move_right();
    b.move_right(); // inside the leading spaces
    b.newline();
    assert_eq!(b.lines, vec!["  ", "      foo"]);
    assert_eq!(b.cursor, (4, 1));
}

#[test]
fn newline_on_blank_indented_line_keeps_indent() {
    let mut b = empty();
    typed(&mut b, "    ");
    b.newline();
    assert_eq!(b.lines, vec!["    ", "    "]);
    assert_eq!(b.cursor, (4, 1));
}

#[test]
fn paste_does_not_auto_indent() {
    let mut b = empty();
    typed(&mut b, "    ab");
    b.newline(); // auto-indents: "    "
    b.insert_multiline("x\n  y");
    // the pasted lines stay verbatim; no indent is injected
    assert_eq!(b.lines, vec!["    ab", "    x", "  y"]);
    assert_eq!(b.cursor, (3, 2));
}

#[test]
fn shift_tab_dedents_current_line() {
    let mut b = empty();
    typed(&mut b, "    foo");
    b.dedent();
    assert_eq!(b.lines, vec!["foo"]);
    assert_eq!(b.cursor, (3, 0));
}

#[test]
fn dedent_without_indent_does_nothing() {
    let mut b = empty();
    b.dedent();
    assert_eq!(b.lines, vec![""]);
    assert!(!b.dirty);
    assert!(!b.undo()); // no undo step was pushed
}

#[test]
fn backspace_merges_lines() {
    let mut b = empty();
    b.insert_text("one");
    b.newline();
    b.insert_text("two");
    b.move_left();
    b.move_left();
    b.move_left();
    b.backspace();
    assert_eq!(b.lines, vec!["onetwo"]);
    assert_eq!(b.cursor, (3, 0));
}

#[test]
fn backspace_at_very_start_does_nothing() {
    let mut b = empty();
    b.backspace();
    assert_eq!(b.lines, vec![""]);
    assert_eq!(b.cursor, (0, 0));
}

#[test]
fn unicode_insert_and_backspace_by_char() {
    let mut b = empty();
    b.insert_text("aé日");
    assert_eq!(b.lines, vec!["aé日"]);
    assert_eq!(b.cursor, (3, 0));
    b.backspace();
    assert_eq!(b.lines, vec!["aé"]);
    // insert in the middle of a multibyte line
    b.home();
    b.move_right();
    b.insert_char('x');
    assert_eq!(b.lines, vec!["axé"]);
}

#[test]
fn tabs_have_editor_width_without_changing_buffer_text() {
    assert_eq!(expand_tabs("\t\tcode"), "        code");
    assert_eq!(char_width('\t'), TAB_WIDTH);

    let mut b = empty();
    b.lines = vec!["\t\tcode".to_string()];
    b.cursor = (1, 0);
    assert_eq!(b.cursor_col(), TAB_WIDTH);
    b.cursor = (2, 0);
    assert_eq!(b.cursor_col(), TAB_WIDTH * 2);
    assert_eq!(b.lines[0], "\t\tcode");
}

#[test]
fn insert_multiline_with_newlines() {
    let mut b = empty();
    typed(&mut b, "ab");
    b.newline();
    typed(&mut b, "cd");
    b.home();
    b.move_down();
    b.insert_multiline("X\nY\nZ");
    assert_eq!(b.lines, vec!["ab", "X", "Y", "Zcd"]);
    // select from line 2 through the end of line 3 and replace with "Q"
    b.home();
    b.move_up();
    b.begin_selection();
    b.move_down();
    b.end();
    b.insert_multiline("Q");
    assert_eq!(b.lines, vec!["ab", "X", "Q"]);
}

#[test]
fn multi_line_edit_tracks_first_changed_line() {
    let mut b = empty();
    typed(&mut b, "abc");
    b.newline();
    typed(&mut b, "def");
    b.home();
    b.insert_multiline("1\n2\n3");
    assert_eq!(b.last_edit_line, Some(0));
}

#[test]
fn consecutive_newlines_merge_but_typing_after_enter_does_not() {
    let mut b = empty();
    b.insert_text("ab");
    b.newline();
    b.insert_text("cd");
    // entries: [before "ab"], [before the newline], [before "cd"]
    b.undo();
    assert_eq!(b.lines, vec!["ab", ""]);
    b.undo();
    assert_eq!(b.lines, vec!["ab"]);
    b.undo();
    assert_eq!(b.lines, vec![""]);
    b.redo();
    b.redo();
    b.redo();
    assert_eq!(b.lines, vec!["ab", "cd"]);

    // but consecutive enters are a single step
    let mut b = empty();
    b.newline();
    b.newline();
    b.undo();
    assert_eq!(b.lines, vec![""]);
}
