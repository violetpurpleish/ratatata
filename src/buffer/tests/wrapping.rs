use super::*;

#[test]
fn chunks_respect_width_and_never_split_wide_chars() {
    // ASCII: rows of at most `width` columns
    let rows: Vec<(usize, usize)> = visual_chunks("abcdefgh", 4).collect();
    assert_eq!(rows, vec![(0, 4), (4, 8)]);
    // wide chars count double: "日本語" is 6 columns, so at width 4
    // two wide chars fit per row and the third starts the next
    let rows: Vec<(usize, usize)> = visual_chunks("日本語", 4).collect();
    assert_eq!(rows, vec![(0, 2), (2, 3)]);
    // a single char wider than the width is kept whole, never split
    let rows: Vec<(usize, usize)> = visual_chunks("日x", 1).collect();
    assert_eq!(rows, vec![(0, 1), (1, 2)]);
    // an empty line is a single empty row
    assert_eq!(visual_chunks("", 10).collect::<Vec<_>>(), vec![(0, 0)]);
    assert_eq!(visual_len("", 10), 1);
    // a zero width degenerates to single-char rows instead of looping
    assert_eq!(visual_chunks("abc", 0).count(), 3);
}

#[test]
fn word_wrap_breaks_at_word_boundaries() {
    // rows end after the last whitespace that fits
    let rows: Vec<(usize, usize)> = visual_chunks("hello world foo", 8).collect();
    assert_eq!(rows, vec![(0, 6), (6, 12), (12, 15)]);
    // a word longer than the width is hard-broken
    let rows: Vec<(usize, usize)> = visual_chunks("ab cdefgh", 4).collect();
    assert_eq!(rows, vec![(0, 3), (3, 7), (7, 9)]);
    // indentation at the start of a line is preserved
    let rows: Vec<(usize, usize)> = visual_chunks("  hello", 5).collect();
    assert_eq!(rows, vec![(0, 2), (2, 7)]);
}

#[test]
fn word_wrap_elides_excess_spaces_at_the_wrap_point() {
    // one space fits on the row, the other two are elided: the second
    // row starts at the first non-whitespace char
    let rows: Vec<(usize, usize)> = visual_chunks("abc    def", 4).collect();
    assert_eq!(rows, vec![(0, 4), (7, 10)]);
    assert_eq!(visual_len("abc    def", 4), 2);
    // a trailing run of spaces ends in one empty row
    assert_eq!(visual_len("abc   ", 3), 2);
}

#[test]
fn wrapped_left_right_skip_elided_spaces() {
    let mut b = empty();
    typed(&mut b, "hello world");
    b.toggle_wrap(5); // rows: "hello" | "world", the space is elided
    // left from "world" lands on "hello"'s last char
    b.cursor = (6, 0);
    b.move_left();
    assert_eq!(b.cursor, (4, 0));
    // and right from there crosses the wrap point onto "world"
    b.move_right();
    assert_eq!(b.cursor, (6, 0));
    // a cursor placed directly on the hidden space is snapped on move
    b.cursor = (5, 0);
    b.move_left();
    assert_eq!(b.cursor, (4, 0));
    b.cursor = (5, 0);
    b.move_right();
    assert_eq!(b.cursor, (6, 0));
}

#[test]
fn wrapped_up_down_never_rest_on_elided_spaces() {
    let mut b = empty();
    typed(&mut b, "hello world");
    b.toggle_wrap(5);
    // up from "world" clamps into "hello"'s last char, not the
    // hidden space at the wrap point
    b.cursor = (6, 0);
    b.move_up();
    assert_eq!(b.cursor, (4, 0));
    // down from "hello" lands on "world"'s first char
    b.cursor = (0, 0);
    b.move_down();
    assert_eq!(b.cursor, (6, 0));
    // End never rests on the hidden space either
    b.cursor = (1, 0);
    b.end();
    assert_eq!(b.cursor, (4, 0));
}

#[test]
fn wrapped_cursor_col_clamps_elided_positions() {
    let mut b = empty();
    typed(&mut b, "hello world");
    b.toggle_wrap(5); // rows: "hello" (0..5) | "world" (6..11)
    b.cursor = (5, 0); // on the elided space
    assert_eq!(b.cursor_col(), 0); // renders at "world"'s start
    b.cursor = (6, 0);
    assert_eq!(b.cursor_col(), 0);
    b.cursor = (10, 0);
    assert_eq!(b.cursor_col(), 4); // end of the wrapped row
}

#[test]
fn wrapped_up_down_follow_visual_rows() {
    let mut b = empty();
    typed(&mut b, "abcdef");
    b.newline();
    typed(&mut b, "xyz");
    b.toggle_wrap(4);
    // visual rows: "abcd" / "ef" / "xyz"

    // down from the first row lands on the second row of the same line
    b.cursor = (2, 0);
    b.move_down();
    assert_eq!(b.cursor, (4, 0));
    // down from the last row moves to the next line (x clamped)
    b.move_down();
    assert_eq!(b.cursor, (3, 1));
    // up from a following line goes to the previous line (x clamped)
    b.move_up();
    assert_eq!(b.cursor, (3, 0));
    // at the top row of a line nothing moves
    b.move_up();
    assert_eq!(b.cursor, (3, 0));
    // from a continuation row, up goes to the previous row of the
    // same line without landing on its exclusive end boundary
    b.cursor = (5, 0);
    b.move_up();
    assert_eq!(b.cursor, (3, 0));
}

#[test]
fn wrapped_left_right_cross_visual_rows() {
    let mut b = empty();
    typed(&mut b, "abcdef");
    b.toggle_wrap(4);
    // at the end of a visual row, right moves onto the next row's
    // first char; left moves back
    b.cursor = (3, 0);
    b.move_right();
    assert_eq!(b.cursor, (4, 0));
    b.move_left();
    assert_eq!(b.cursor, (3, 0));
    // at the start of a continuation row, left goes to the previous
    // row's last char (plain char movement crosses the wrap point)
    b.cursor = (5, 0);
    b.move_left();
    assert_eq!(b.cursor, (4, 0));
}

#[test]
fn wrapped_home_end_are_visual() {
    let mut b = empty();
    typed(&mut b, "abcdef");
    b.toggle_wrap(4);
    // Home lands at the start of the visual row, not the line
    b.cursor = (5, 0);
    b.home();
    assert_eq!(b.cursor, (4, 0));
    b.end();
    assert_eq!(b.cursor, (6, 0));
    // End stops at the end of the visual row on the first row
    b.cursor = (1, 0);
    b.end();
    assert_eq!(b.cursor, (4, 0));
    // Home from the row boundary stays at the boundary: it is the
    // start of the second visual row
    b.home();
    assert_eq!(b.cursor, (4, 0));
    b.home();
    assert_eq!(b.cursor, (4, 0));
}

#[test]
fn wrapped_page_moves_by_visual_rows() {
    let mut b = empty();
    typed(&mut b, "abcdefghij"); // 2 rows at width 5
    b.newline();
    typed(&mut b, "xyz");
    b.toggle_wrap(5);
    b.cursor = (0, 0);
    // one page down: the second row of the same line
    b.page_down(1);
    assert_eq!(b.cursor, (5, 0));
    // another: the next line (x clamps into the shorter row)
    b.page_down(1);
    assert_eq!(b.cursor, (3, 1));
    b.page_up(1);
    assert_eq!(b.cursor, (5, 0));
    b.page_up(10);
    assert_eq!(b.cursor, (5, 0)); // clamped to the top row
    b.page_down(100);
    assert_eq!(b.cursor, (3, 1)); // clamped to the last row
}

#[test]
fn wrapped_ensure_visible_scrolls_visual_rows() {
    let mut b = empty();
    typed(&mut b, "abcdefghij"); // 2 rows at width 5
    for _ in 0..3 {
        b.newline();
        typed(&mut b, "k");
    }
    // visual rows: line 0 -> 2 rows, lines 1..=3 -> 1 row each
    b.toggle_wrap(5);
    // cursor vrow 5 with a 2-row viewport scrolls to vrow 3
    b.cursor = (0, 3);
    b.ensure_visible(2, 5);
    assert_eq!(b.scroll.1, 3);
    assert_eq!(b.scroll.0, 0); // horizontal scroll is meaningless
    // a cursor on the second row of a wrapped line scrolls to it
    b.cursor = (7, 0);
    b.ensure_visible(1, 5);
    assert_eq!(b.scroll.1, 1);
    b.cursor = (0, 0);
    b.ensure_visible(2, 5);
    assert_eq!(b.scroll.1, 0);
}

#[test]
fn wrapped_cursor_col_is_within_the_visual_row() {
    let mut b = empty();
    typed(&mut b, "日本語"); // 6 columns
    b.toggle_wrap(4); // rows: "日本" (4 cols) | "語" (2 cols)
    b.cursor = (0, 0);
    assert_eq!(b.cursor_col(), 0);
    b.cursor = (1, 0);
    assert_eq!(b.cursor_col(), 2);
    b.cursor = (2, 0);
    assert_eq!(b.cursor_col(), 0); // start of the wrapped row
    b.cursor = (3, 0);
    assert_eq!(b.cursor_col(), 2); // end of the wrapped row
}

#[test]
fn vrow_mapping_and_total_rows() {
    let mut b = empty();
    typed(&mut b, "abcdefghij"); // 2 rows at width 5
    b.newline();
    typed(&mut b, "xyz");
    b.toggle_wrap(5);
    b.cursor = (0, 0);
    assert_eq!(b.total_visual_rows(), 3);
    assert_eq!(b.cursor_vrow(), 0);
    assert_eq!(b.vrow_position(0), Some((0, 0)));
    assert_eq!(b.vrow_position(1), Some((0, 1)));
    assert_eq!(b.vrow_position(2), Some((1, 0)));
    assert_eq!(b.vrow_position(3), None);
    b.cursor = (7, 0);
    assert_eq!(b.cursor_vrow(), 1);
    // without wrapping, vrows are plain line numbers
    b.toggle_wrap(5);
    assert_eq!(b.total_visual_rows(), 2);
    assert_eq!(b.vrow_position(1), Some((1, 0)));
}
