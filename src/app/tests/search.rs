use super::*;

#[test]
fn ctrl_f_opens_search_and_typing_highlights_matches() {
    let dir = scratch("search1");
    let file = dir.join("a.txt");
    fs::write(&file, "hello world\nhello again\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();

    app.handle_key(ctrl('f'));
    assert!(app.search.is_some());
    assert_eq!(app.search.as_ref().unwrap().query, "");

    open_search_typed(&mut app, "hello");
    let search = app.search.as_ref().unwrap();
    assert_eq!(search.query, "hello");
    assert_eq!(search.match_count(), 2);
    assert_eq!(search.current_index(), 0);
    // the cursor jumped to the current match...
    assert_eq!(app.buffer.cursor, (0, 0));
    // ...and the buffer itself was not edited
    assert_eq!(app.buffer.lines, vec!["hello world", "hello again", ""]);
}

#[test]
fn search_enter_and_shift_enter_step_through_matches() {
    let dir = scratch("search2");
    let file = dir.join("a.txt");
    fs::write(&file, "aa bb aa\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    open_search_typed(&mut app, "aa");

    app.handle_key(key(KeyCode::Enter));
    let search = app.search.as_ref().unwrap();
    assert_eq!(search.current_index(), 1);
    assert_eq!(app.buffer.cursor, (6, 0));

    // wraps around past the last match
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.buffer.cursor, (0, 0));

    // Shift+Enter goes back (kitty terminals report it as Enter+SHIFT)
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    assert_eq!(app.buffer.cursor, (6, 0));

    // Ctrl+F again jumps to the next match too
    app.handle_key(ctrl('f'));
    assert_eq!(app.buffer.cursor, (0, 0));
    assert!(app.search.is_some());
}

#[test]
fn search_backspace_and_esc() {
    let dir = scratch("search3");
    let file = dir.join("a.txt");
    fs::write(&file, "foo bar\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    open_search_typed(&mut app, "foob");
    assert_eq!(app.search.as_ref().unwrap().match_count(), 0);

    app.handle_key(key(KeyCode::Backspace)); // "foo" now matches
    let search = app.search.as_ref().unwrap();
    assert_eq!(search.query, "foo");
    assert_eq!(search.match_count(), 1);
    assert_eq!(
        search.current_match(),
        Some(crate::search::Match {
            line: 0,
            start: 0,
            end: 3
        })
    );

    app.handle_key(key(KeyCode::Esc));
    assert!(app.search.is_none());
}

#[test]
fn editor_navigation_dismisses_search() {
    let dir = scratch("search-navigation");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha\nbeta\ncharlie\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();

    open_search_typed(&mut app, "charlie");
    assert_eq!(app.buffer.cursor, (0, 2));
    assert!(app.search.is_some());

    // Search stays open after matching, but an editor navigation key must
    // not be swallowed by the search prompt.
    app.handle_key(key(KeyCode::Up));
    assert!(app.search.is_none());
    assert_eq!(app.buffer.cursor, (0, 1));
}

#[test]
fn wrapped_search_click_then_up_continues_moving() {
    let dir = scratch("search-wrapped-click");
    let file = dir.join("notes.txt");
    // Self-contained fixture: a few short lines, then a wrapping line
    // with the search term on a continuation visual row. The previous
    // version opened README.md and hardcoded a cursor line, which
    // drifted whenever the docs changed.
    let long_line = "Installs as the `rat` command (the binary name is set explicitly in Cargo.toml, separate from the package name). Requires a Rust toolchain with Edition 2024 support. Works in any terminal that supports crossterm event and drawing APIs.";
    let contents = format!(
        "{}\n{long_line}\n",
        (0..8)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    fs::write(&file, &contents).unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();

    render_sized(&mut app, 60, 24); // establish the viewport width
    app.handle_key(ctrl('w'));
    render_sized(&mut app, 60, 24); // establish the wrapped rows

    let query = "command";
    let match_line = app
        .buffer
        .lines
        .iter()
        .position(|line| line.contains(query))
        .expect("fixture contains the search term");
    let match_start = app.buffer.lines[match_line]
        .find(query)
        .expect("search term is on that line");
    let wrap_width = app.buffer.wrap_width.max(1);
    let match_vrow =
        crate::buffer::visual_row_of(&app.buffer.lines[match_line], match_start, wrap_width);
    assert!(
        match_vrow > 0,
        "fixture must wrap so the match sits on a continuation row"
    );
    assert!(match_line > 0, "fixture needs a previous logical line");

    open_search_typed(&mut app, query);
    app.handle_key(key(KeyCode::Enter));
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(key(KeyCode::Up));
    // Up from a continuation row stays on the same logical line.
    assert_eq!(app.buffer.cursor.1, match_line);
    assert_eq!(
        crate::buffer::visual_row_of(
            &app.buffer.lines[match_line],
            app.buffer.cursor.0,
            wrap_width
        ),
        match_vrow - 1
    );

    // Redraw after the first move, then click a few characters into the
    // only search result as a user would.
    render_sized(&mut app, 60, 24);
    let target = (match_start + 3, match_line);
    let vrow = {
        let old = app.buffer.cursor;
        app.buffer.cursor = target;
        let row = app.buffer.cursor_vrow();
        app.buffer.cursor = old;
        row
    };
    let inner_x = app.editor_area.x as usize + 1;
    let inner_y = app.editor_area.y as usize + 1;
    let gutter_w = app.buffer.lines.len().to_string().len() + 1;
    let (chunk_start, _) = crate::buffer::visual_chunk(
        &app.buffer.lines[target.1],
        vrow - app.buffer.lines[..target.1]
            .iter()
            .map(|line| crate::buffer::visual_len(line, app.buffer.wrap_width))
            .sum::<usize>(),
        app.buffer.wrap_width,
    );
    let click_x = inner_x + gutter_w + target.0 - chunk_start;
    let click_y = inner_y + vrow - app.buffer.scroll.1;
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        click_x as u16,
        click_y as u16,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        click_x as u16,
        click_y as u16,
    ));
    assert_eq!(app.buffer.cursor, target);

    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.buffer.cursor.1, match_line);
    assert_eq!(
        crate::buffer::visual_row_of(
            &app.buffer.lines[match_line],
            app.buffer.cursor.0,
            wrap_width
        ),
        match_vrow - 1
    );
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.buffer.cursor.1, match_line - 1);
}

#[test]
fn search_without_matches_keeps_cursor_put() {
    let dir = scratch("search4");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha beta\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    app.buffer.cursor = (3, 0);
    open_search_typed(&mut app, "zzz");
    let search = app.search.as_ref().unwrap();
    assert_eq!(search.match_count(), 0);
    assert_eq!(app.buffer.cursor, (3, 0));
    // the status bar shows "no matches"
    let rows = render(&mut app);
    assert!(row_contains(&rows, "no matches"));
}

#[test]
fn search_rendering_highlights_matches_and_counts() {
    let dir = scratch("search5");
    let file = dir.join("a.txt");
    fs::write(&file, "hello world\nhello again\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app); // set the viewport first
    open_search_typed(&mut app, "hello");

    let buf = render_buffer(&mut app);
    // current match (line 0, chars 0..5): yellow background
    for x in 31..36 {
        assert_eq!(
            buf.cell((x, 3)).unwrap().style().bg,
            Some(SEARCH_CURRENT_BG),
            "col {x}"
        );
    }
    // the other match (line 1, chars 0..5): the dim match color
    for x in 31..36 {
        assert_eq!(
            buf.cell((x, 4)).unwrap().style().bg,
            Some(SEARCH_OTHER_BG),
            "col {x}"
        );
    }
    // outside the matches: untouched (the theme's base background,
    // like every plain cell)
    assert_eq!(buf.cell((36, 3)).unwrap().style().bg, Some(PALETTE.bg));
    assert_eq!(buf.cell((31, 5)).unwrap().style().bg, Some(PALETTE.bg));

    // the status bar shows the prompt, the query and the counter
    let rows = render(&mut app);
    assert!(row_contains(&rows, "search:"));
    assert!(row_contains(&rows, "hello"));
    assert!(row_contains(&rows, "1/2"));
}

#[test]
fn mouse_click_dismisses_search() {
    let dir = scratch("search6");
    let file = dir.join("a.txt");
    fs::write(&file, "hello world\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_buffer(&mut app);
    open_search_typed(&mut app, "hello");
    assert!(app.search.is_some());

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 35, 2));
    assert!(app.search.is_none());
    // and the click still moves the cursor (char 4 of line 0)
    assert_eq!(app.buffer.cursor, (4, 0));
}

#[test]
fn ctrl_v_pastes_into_the_search_query() {
    let dir = scratch("search7");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha\n").unwrap();
    let mut app = with_fake_clipboard(new_app(dir, Some(file)).unwrap());
    app.clipboard.set_text("alp").unwrap();
    app.handle_key(ctrl('f'));
    app.handle_key(ctrl('v'));
    assert_eq!(app.search.as_ref().unwrap().query, "alp");
    assert_eq!(app.search.as_ref().unwrap().match_count(), 1);
    assert_eq!(app.buffer.lines, vec!["alpha", ""]); // buffer untouched
}

#[test]
fn ctrl_z_while_searching_does_not_undo_the_buffer() {
    let dir = scratch("search8");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    app.handle_key(char_key('X')); // buffer now "Xalpha"
    open_search_typed(&mut app, "X");
    assert_eq!(app.buffer.lines, vec!["Xalpha", ""]);

    app.handle_key(ctrl('z'));
    assert_eq!(app.buffer.lines, vec!["Xalpha", ""]);
    assert!(app.search.is_some());
}

#[test]
fn ctrl_shift_h_opens_replace_and_replaces_current_then_all() {
    let dir = scratch("replace1");
    let file = dir.join("a.txt");
    fs::write(&file, "foo bar foo\nfoo\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();

    app.handle_key(ctrl_shift('h'));
    assert!(app.search.as_ref().is_some_and(|s| s.is_replace()));
    assert_eq!(
        app.search.as_ref().unwrap().field,
        crate::search::SearchField::Query
    );
    for c in "foo".chars() {
        app.handle_key(char_key(c));
    }
    assert_eq!(app.search.as_ref().unwrap().match_count(), 3);

    // Enter on the find field moves to the replacement
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.search.as_ref().unwrap().field,
        crate::search::SearchField::Replacement
    );
    for c in "qux".chars() {
        app.handle_key(char_key(c));
    }
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.buffer.lines, vec!["qux bar foo", "foo", ""]);
    assert_eq!(app.search.as_ref().unwrap().match_count(), 2);

    // Shift+Enter replaces the rest
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    assert_eq!(app.buffer.lines, vec!["qux bar qux", "qux", ""]);
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(msg, _)| msg.contains("replaced 2"))
    );

    app.handle_key(key(KeyCode::Esc));
    assert!(app.search.is_none());
}

#[test]
fn replace_reuses_an_open_search_query() {
    let dir = scratch("replace2");
    let file = dir.join("a.txt");
    fs::write(&file, "hello hello\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    open_search_typed(&mut app, "hello");
    app.handle_key(ctrl_shift('h'));
    let search = app.search.as_ref().unwrap();
    assert!(search.is_replace());
    assert_eq!(search.query, "hello");
    assert_eq!(search.field, crate::search::SearchField::Replacement);

    for c in "hi".chars() {
        app.handle_key(char_key(c));
    }
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.buffer.lines[0], "hi hello");
}

#[test]
fn replace_with_no_matches_is_a_status_message() {
    let dir = scratch("replace3");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    app.handle_key(ctrl_shift('h'));
    for c in "zzz".chars() {
        app.handle_key(char_key(c));
    }
    app.handle_key(key(KeyCode::Tab));
    app.handle_key(char_key('x'));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.buffer.lines, vec!["alpha", ""]);
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(msg, _)| msg.contains("no matches"))
    );
}

#[test]
fn replace_undoes_one_replacement_and_replace_all_as_one_step() {
    let dir = scratch("replace4");
    let file = dir.join("a.txt");
    fs::write(&file, "aa aa aa").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    app.handle_key(ctrl_shift('h'));
    app.handle_key(char_key('a'));
    app.handle_key(char_key('a'));
    app.handle_key(key(KeyCode::Tab));
    app.handle_key(char_key('b'));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.buffer.lines, vec!["b aa aa"]);
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(ctrl('z'));
    assert_eq!(app.buffer.lines, vec!["aa aa aa"]);

    app.handle_key(ctrl_shift('h'));
    app.handle_key(char_key('a'));
    app.handle_key(char_key('a'));
    app.handle_key(key(KeyCode::Tab));
    app.handle_key(char_key('b'));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    assert_eq!(app.buffer.lines, vec!["b b b"]);
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(ctrl('z'));
    assert_eq!(app.buffer.lines, vec!["aa aa aa"]);
}

#[test]
fn ctrl_h_during_replace_still_toggles_dotfiles() {
    let dir = scratch("replace5");
    fs::write(dir.join(".env"), "SECRET=1").unwrap();
    fs::write(dir.join("a.txt"), "foo").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    app.handle_key(ctrl_shift('h'));
    assert!(app.sidebar.hide_dotfiles);
    app.handle_key(ctrl('h'));
    assert!(!app.sidebar.hide_dotfiles);
    assert!(app.search.as_ref().is_some_and(|s| s.is_replace()));
}

#[test]
fn cmd_shift_h_opens_replace_like_ctrl_shift_h() {
    let dir = scratch("replace6");
    fs::write(dir.join("a.txt"), "foo").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    app.handle_key(cmd_shift('h'));
    assert!(app.search.as_ref().is_some_and(|s| s.is_replace()));
}

#[test]
fn ghostty_ctrl_shift_h_is_replace_not_hide_dotfiles() {
    // Ghostty with the kitty protocol and REPORT_ALTERNATE_KEYS folds
    // Shift into the character and drops the SHIFT modifier, so
    // Ctrl+Shift+H arrives as 'H'+CONTROL — the same shape as
    // Ctrl+Shift+Z. That must open replace, not toggle hide-dotfiles.
    let events = [
        KeyEvent::new(KeyCode::Char('H'), KeyModifiers::CONTROL),
        KeyEvent::new(
            KeyCode::Char('H'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        KeyEvent::new(
            KeyCode::Char('h'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SUPER),
        KeyEvent::new(
            KeyCode::Char('h'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ),
    ];
    for (i, event) in events.into_iter().enumerate() {
        let dir = scratch(&format!("ghostty-csh-{i}"));
        fs::write(dir.join(".env"), "SECRET=1").unwrap();
        fs::write(dir.join("a.txt"), "hello").unwrap();
        let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        assert!(app.sidebar.hide_dotfiles);
        app.handle_key(event);
        assert!(
            app.search.as_ref().is_some_and(|s| s.is_replace()),
            "event {event:?} must open replace"
        );
        assert!(
            app.sidebar.hide_dotfiles,
            "event {event:?} must not toggle hide-dotfiles"
        );
    }
}

#[test]
fn unshifted_ctrl_h_still_toggles_dotfiles_not_replace() {
    let dir = scratch("ctrl-h-not-replace");
    fs::write(dir.join(".env"), "SECRET=1").unwrap();
    fs::write(dir.join("a.txt"), "hello").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    app.handle_key(ctrl('h'));
    assert!(!app.sidebar.hide_dotfiles);
    assert!(app.search.is_none());
    app.handle_key(cmd('h'));
    assert!(app.sidebar.hide_dotfiles);
    assert!(app.search.is_none());
}

#[test]
fn ctrl_g_jumps_to_a_1_based_line_and_keeps_it_visible() {
    let dir = scratch("goto1");
    let file = dir.join("a.txt");
    let contents: String = (1..=40).map(|n| format!("line {n}\n")).collect();
    fs::write(&file, &contents).unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    render_sized(&mut app, 80, 12);

    app.handle_key(ctrl('g'));
    assert_eq!(app.goto_line_input.as_deref(), Some(""));
    let rows = render(&mut app);
    assert!(row_contains(&rows, "go to line:"));

    app.handle_key(char_key('3'));
    app.handle_key(char_key('5'));
    app.handle_key(key(KeyCode::Enter));
    assert!(app.goto_line_input.is_none());
    assert_eq!(app.buffer.cursor, (0, 34));
    assert_eq!(app.focus, Focus::Editor);
    // the jumped-to line must be inside the viewport
    let (_w, h) = app.editor_text;
    assert!(app.buffer.cursor.1 >= app.buffer.scroll.1);
    assert!(app.buffer.cursor.1 < app.buffer.scroll.1 + h as usize);
}

#[test]
fn ctrl_g_invalid_input_is_a_status_message() {
    let dir = scratch("goto2");
    let file = dir.join("a.txt");
    fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    let cursor = app.buffer.cursor;

    app.handle_key(ctrl('g'));
    for c in "nope".chars() {
        app.handle_key(char_key(c));
    }
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.buffer.cursor, cursor);
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(msg, _)| msg.contains("invalid line number"))
    );

    app.handle_key(ctrl('g'));
    app.handle_key(char_key('0'));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.buffer.cursor, cursor);
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(msg, _)| msg.contains("out of range") || msg.contains("invalid"))
    );

    app.handle_key(ctrl('g'));
    for c in "99".chars() {
        app.handle_key(char_key(c));
    }
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.buffer.cursor, cursor);
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(msg, _)| msg.contains("out of range"))
    );

    app.handle_key(ctrl('g'));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.goto_line_input.is_none());
}

#[test]
fn clip_ops_styles_search_matches() {
    let line = "hello world";
    let style = Style::default().fg(Color::Blue);
    let spans = clip_ops(
        line,
        &[(Some(style), 0..line.len())],
        0,
        line.chars().count(),
        None,
        &[(0, 5, false)],
        ColorSupport::TrueColor,
    );
    let parts: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(parts, vec!["hello", " world"]);
    // match keeps the syntax fg and gains the match background
    assert_eq!(spans[0].style.bg, Some(SEARCH_OTHER_BG));
    assert_eq!(spans[0].style.fg, Some(Color::Blue));
    assert_eq!(spans[1].style.bg, None);
}

#[test]
fn clicking_search_button_opens_search() {
    let dir = scratch("mbtnsearch");
    fs::write(dir.join("a.txt"), "hello world\nhello again\n").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    render_buffer(&mut app);

    click_button(&mut app, Shortcut::Find);
    assert!(app.search.is_some());
    // typing narrows the query; clicking again steps to the next
    // match, exactly like Ctrl+F
    for c in "hello".chars() {
        app.handle_key(char_key(c));
    }
    assert_eq!(app.buffer.cursor, (0, 0)); // first match
    click_button(&mut app, Shortcut::Find);
    assert_eq!(app.buffer.cursor, (0, 1)); // second match
}

#[test]
fn clicking_line_button_opens_the_goto_prompt() {
    let dir = scratch("mbtnline");
    fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    render_buffer(&mut app);
    click_button(&mut app, Shortcut::GoToLine);
    assert_eq!(app.goto_line_input.as_deref(), Some(""));
    app.handle_key(char_key('3'));
    app.handle_key(key(KeyCode::Enter));
    assert!(app.goto_line_input.is_none());
    assert_eq!(app.buffer.cursor, (0, 2));
}

#[test]
fn clicking_replace_button_opens_find_and_replace() {
    let dir = scratch("mbtnreplace");
    fs::write(dir.join("a.txt"), "foo bar foo\n").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    render_buffer(&mut app);
    click_button(&mut app, Shortcut::Replace);
    assert!(app.search.as_ref().is_some_and(|s| s.is_replace()));
    for c in "foo".chars() {
        app.handle_key(char_key(c));
    }
    app.handle_key(key(KeyCode::Tab));
    for c in "qux".chars() {
        app.handle_key(char_key(c));
    }
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.buffer.lines[0], "qux bar foo");
}

#[test]
fn select_all_copy_and_paste_replaces() {
    let dir = scratch("clip2");
    let mut app = with_fake_clipboard(new_app(dir, None).unwrap());
    app.handle_key(ctrl('o'));
    for c in "abc".chars() {
        app.handle_key(char_key(c));
    }
    app.handle_key(key(KeyCode::Enter));
    for c in "def".chars() {
        app.handle_key(char_key(c));
    }
    app.handle_key(ctrl('a'));
    app.handle_key(ctrl('c'));
    assert_eq!(app.clipboard.get_text().as_deref(), Some("abc\ndef"));

    // pasting over the selection replaces it (no duplication)
    app.handle_key(ctrl('v'));
    assert_eq!(app.buffer.lines, vec!["abc", "def"]);
    assert!(!app.buffer.has_selection());
}

#[test]
fn paste_with_selection_replaces_it() {
    let dir = scratch("clip5");
    let mut app = with_fake_clipboard(new_app(dir, None).unwrap());
    app.handle_key(ctrl('o'));
    for c in "abcdef".chars() {
        app.handle_key(char_key(c));
    }
    app.handle_key(key(KeyCode::Home));
    for _ in 0..3 {
        app.handle_key(shift_key(KeyCode::Right));
    }
    app.clipboard.set_text("XYZ").unwrap();
    app.handle_key(ctrl('v'));
    assert_eq!(app.buffer.lines, vec!["XYZdef"]);
}
