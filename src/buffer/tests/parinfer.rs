use super::*;

#[test]
fn parinfer_typing_open_paren_inserts_close_and_keeps_cursor() {
    let mut b = clj_buf();
    type_clj(&mut b, "(");
    assert_eq!(b.lines, vec!["()"]);
    assert_eq!(b.cursor, (1, 0));
}

#[test]
fn parinfer_does_not_run_on_plain_text_buffers() {
    let mut b = Buffer::empty_at(PathBuf::from("notes.txt"));
    b.insert_char('(');
    b.apply_parinfer();
    assert_eq!(b.lines, vec!["("]);
    assert_eq!(b.cursor, (1, 0));
}

#[test]
fn parinfer_does_not_run_on_untitled_buffers() {
    let mut b = empty();
    b.insert_char('(');
    b.apply_parinfer();
    assert_eq!(b.lines, vec!["("]);
}

#[test]
fn parinfer_indent_change_moves_closing_paren() {
    let mut b = clj_buf();
    b.lines = vec!["(foo)".to_string(), "bar".to_string()];
    b.cursor = (0, 1);
    b.sync_parinfer_prev();
    b.indent();
    b.apply_parinfer();
    assert_eq!(b.lines, vec!["(foo", "    bar)"]);
    assert_eq!(b.cursor, (4, 1));
}

#[test]
fn parinfer_indent_on_crlf_file_moves_closing_paren() {
    let path = tmp_path("parinfer-crlf.clj");
    std::fs::write(&path, "(foo)\r\nbar").unwrap();
    let mut b = Buffer::from_path(path).unwrap();
    assert_eq!(b.lines, vec!["(foo)", "bar"]);
    b.cursor = (0, 1);
    b.sync_parinfer_prev();
    b.indent();
    b.apply_parinfer();
    assert_eq!(b.lines, vec!["(foo", "    bar)"]);
    assert_eq!(b.cursor, (4, 1));
}

#[test]
fn parinfer_newline_inside_form_preserves_cursor_line() {
    let mut b = clj_buf();
    type_clj(&mut b, "(foo");
    assert_eq!(b.lines, vec!["(foo)"]);
    assert_eq!(b.cursor, (4, 0));
    let scroll = b.scroll;
    b.newline();
    b.apply_parinfer();
    assert_eq!(b.cursor.1, 1);
    assert_eq!(b.lines.len(), 2);
    assert!(b.lines[0].starts_with("(foo"));
    assert_eq!(b.scroll, scroll, "parinfer must not reset the viewport");
}

#[test]
fn parinfer_incomplete_string_does_not_drop_input() {
    let mut b = clj_buf();
    type_clj(&mut b, "\"hello");
    assert_eq!(b.lines, vec!["\"hello"]);
    assert_eq!(b.cursor, (6, 0));
}

#[test]
fn parinfer_user_edit_and_adjustment_undo_as_one_step() {
    let mut b = clj_buf();
    type_clj(&mut b, "(");
    assert_eq!(b.lines, vec!["()"]);
    assert!(b.undo());
    assert_eq!(b.lines, vec![""]);
    assert_eq!(b.cursor, (0, 0));
    assert!(b.redo());
    assert_eq!(b.lines, vec!["()"]);
    assert_eq!(b.cursor, (1, 0));
}

#[test]
fn parinfer_continuous_typing_stays_one_undo_step() {
    let mut b = clj_buf();
    type_clj(&mut b, "(def");
    assert_eq!(b.lines, vec!["(def)"]);
    b.undo();
    assert_eq!(b.lines, vec![""]);
    b.redo();
    assert_eq!(b.lines, vec!["(def)"]);
}

#[test]
fn parinfer_edn_files_are_enabled() {
    let mut b = Buffer::empty_at(PathBuf::from("data.edn"));
    type_clj(&mut b, "[");
    assert_eq!(b.lines, vec!["[]"]);
    assert_eq!(b.cursor, (1, 0));
}

#[test]
fn parinfer_preserves_selection_when_line_is_unchanged() {
    let mut b = clj_buf();
    type_clj(&mut b, "(foo bar)");
    b.home();
    b.move_right();
    b.begin_selection();
    b.move_right();
    b.move_right();
    b.move_right(); // "foo"
    assert_eq!(b.selected_text().as_deref(), Some("foo"));
    let scroll = b.scroll;
    b.sync_parinfer_cursor();
    assert_eq!(b.selected_text().as_deref(), Some("foo"));
    assert_eq!(b.scroll, scroll);
}

#[test]
fn parinfer_select_all_syncs_prev_cursor_without_rewriting() {
    let mut b = clj_buf();
    b.lines = vec!["(foo)".to_string(), "(bar)".to_string()];
    b.cursor = (0, 0);
    b.sync_parinfer_prev();
    b.select_all();
    assert_eq!(b.cursor, (5, 1));
    assert_eq!(b.parinfer_prev_cursor(), (5, 1));
    assert_eq!(b.lines, vec!["(foo)", "(bar)"]);
    assert!(!b.dirty);
}

#[test]
fn parinfer_cursor_only_navigation_does_not_rewrite_or_create_undo() {
    let mut b = clj_buf();
    // Smart Mode would close this if it ran on cursor movement.
    b.lines = vec!["(foo".to_string(), "bar".to_string()];
    b.cursor = (4, 0);
    b.sync_parinfer_prev();
    let before = b.lines.clone();

    b.move_down();
    b.sync_parinfer_cursor();
    b.move_right();
    b.sync_parinfer_cursor();
    b.home();
    b.sync_parinfer_cursor();
    b.end();
    b.sync_parinfer_cursor();
    b.move_up();
    b.sync_parinfer_cursor();
    b.page_down(10);
    b.sync_parinfer_cursor();
    b.page_up(10);
    b.sync_parinfer_cursor();

    assert_eq!(b.lines, before);
    assert!(!b.dirty);
    assert!(!b.undo(), "cursor movement must not create an undo entry");

    // A mistaken engine call after navigation must still not rewrite.
    b.apply_parinfer();
    assert_eq!(b.lines, before);
    assert!(!b.dirty);
}

#[test]
fn parinfer_insert_and_delete_after_unicode_clusters() {
    for prefix in [CJK, COMBINING, ZWJ] {
        let mut b = clj_buf();
        // Insert the whole cluster at once so ZWJ sequences are not
        // split across Smart Mode requests.
        b.insert_text(prefix);
        b.apply_parinfer();
        let n = prefix.chars().count();
        assert_eq!(b.lines, vec![prefix.to_string()], "{prefix:?}");
        assert_eq!(b.cursor, (n, 0), "{prefix:?}");
        assert_cursor_in_range(&b);

        b.insert_char('(');
        b.apply_parinfer();
        assert_eq!(b.lines, vec![format!("{prefix}()")], "{prefix:?}");
        assert_eq!(b.cursor, (n + 1, 0), "{prefix:?}");
        assert_cursor_in_range(&b);
        if prefix == COMBINING {
            assert_ne!(b.cursor.0, 1, "cursor must not sit on the combining mark");
        }

        b.backspace();
        b.apply_parinfer();
        assert!(b.lines[0].starts_with(prefix), "{prefix:?} {}", b.lines[0]);
        assert_cursor_in_range(&b);
        if prefix == COMBINING {
            assert_ne!(b.cursor.0, 1, "cursor must not sit on the combining mark");
        }

        let mut b = clj_buf();
        b.insert_text(&format!("{prefix}x"));
        b.apply_parinfer();
        assert_eq!(b.cursor, (n + 1, 0), "{prefix:?}");
        b.backspace();
        b.apply_parinfer();
        assert_eq!(b.lines, vec![prefix.to_string()], "{prefix:?}");
        assert_eq!(b.cursor, (n, 0), "{prefix:?}");
        assert_cursor_in_range(&b);

        b.home();
        b.sync_parinfer_cursor();
        b.delete();
        b.apply_parinfer();
        assert_cursor_in_range(&b);
    }
}

#[test]
fn parinfer_indent_after_unicode_prefixes_moves_closing_paren() {
    for prefix in [CJK, COMBINING, ZWJ] {
        let mut b = clj_buf();
        b.lines = vec![format!("{prefix}(foo)"), "bar".to_string()];
        b.cursor = (0, 1);
        b.sync_parinfer_prev();
        b.indent();
        b.apply_parinfer();
        assert_eq!(
            b.lines,
            vec![format!("{prefix}(foo"), "    bar)".to_string()],
            "{prefix:?}"
        );
        assert_eq!(b.cursor, (4, 1), "{prefix:?}");
        assert_cursor_in_range(&b);
    }
}
