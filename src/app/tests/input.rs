use super::*;

#[test]
fn ctrl_o_toggles_focus() {
    let dir = scratch("toggle");
    let mut app = new_app(dir, None).unwrap();
    assert_eq!(app.focus, Focus::Sidebar);
    app.handle_key(ctrl('o'));
    assert_eq!(app.focus, Focus::Editor);
    app.handle_key(ctrl('o'));
    assert_eq!(app.focus, Focus::Sidebar);
}

#[test]
fn ctrl_b_hides_the_sidebar_and_preserves_selection() {
    let dir = scratch("sidebar-toggle1");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    fs::write(dir.join("b.txt"), "beta").unwrap();
    let mut app = new_app(dir.clone(), None).unwrap();
    app.sidebar.select_name("b.txt");
    let selected = app.sidebar.selected;
    let listed = app.sidebar.dir.clone();

    app.handle_key(ctrl('b'));
    assert!(!app.sidebar_visible);
    assert_eq!(app.focus, Focus::Editor); // was sidebar; don't trap
    assert_eq!(app.sidebar.selected, selected);
    assert_eq!(app.sidebar.dir, listed);

    render_buffer(&mut app);
    let rows = render(&mut app);
    assert!(!row_contains(&rows, "b.txt"));
    assert!(app.editor_area.x < 28);

    app.handle_key(ctrl('b'));
    assert!(app.sidebar_visible);
    assert_eq!(app.sidebar.selected, selected);
    render_buffer(&mut app);
    let rows = render(&mut app);
    assert!(row_contains(&rows, "b.txt"));
}

#[test]
fn ctrl_o_while_sidebar_hidden_shows_and_focuses_it() {
    let dir = scratch("sidebar-toggle2");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    assert_eq!(app.focus, Focus::Editor);
    app.handle_key(ctrl('b'));
    assert!(!app.sidebar_visible);
    assert_eq!(app.focus, Focus::Editor);

    app.handle_key(ctrl('o'));
    assert!(app.sidebar_visible);
    assert_eq!(app.focus, Focus::Sidebar);

    app.handle_key(ctrl('o'));
    assert!(app.sidebar_visible);
    assert_eq!(app.focus, Focus::Editor);
}

#[test]
fn shift_modified_char_inserts_into_buffer() {
    let dir = scratch("shiftchar");
    let file = dir.join("a.txt");
    fs::write(&file, "").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();

    // Some terminals report Shift+letter as the base key plus a SHIFT
    // modifier; the ASCII letter is folded to uppercase.
    app.handle_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
    app.handle_key(shift_key(KeyCode::Char('a')));
    assert_eq!(app.buffer.lines, vec!["AA"]);
}

#[test]
fn alt_modified_char_inserts_into_buffer() {
    // German macOS layout: `[`/`]` are Option+5 / Option+6 and
    // `{`/`}` are Option+8 / Option+9.
    let dir = scratch("altchar");
    let file = dir.join("a.txt");
    fs::write(&file, "").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();

    app.handle_key(alt('['));
    app.handle_key(alt(']'));
    app.handle_key(alt('{'));
    app.handle_key(alt('}'));
    assert_eq!(app.buffer.lines, vec!["[]{}"]);
}

#[test]
fn alt_shift_modified_char_inserts_into_buffer() {
    // e.g. Option+Shift+7 on a German macOS layout produces `|`.
    let dir = scratch("altshiftchar");
    let file = dir.join("a.txt");
    fs::write(&file, "").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();

    app.handle_key(KeyEvent::new(
        KeyCode::Char('|'),
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert_eq!(app.buffer.lines, vec!["|"]);
}

#[test]
fn plain_text_chars_insert_as_reported() {
    // What a kitty-protocol terminal (e.g. Ghostty on macOS) sends for
    // ordinary printable keys once REPORT_ALL_KEYS_AS_ESCAPE_CODES is
    // not requested: the resulting character as plain text, with no
    // modifiers. German layout: Shift+8 is `(`, Option+5 is `[`,
    // Option+8 is `{`, Option+9 is `}`, Option+L is `@`.
    let dir = scratch("plainchars");
    let file = dir.join("a.txt");
    fs::write(&file, "").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();

    for c in ['a', 'A', '(', '[', '{', '}', '@', ']', ')'] {
        app.handle_key(char_key(c));
    }
    assert_eq!(app.buffer.lines, vec!["aA([{}@])"]);
}

#[test]
fn ctrl_and_super_chars_do_not_insert() {
    let dir = scratch("ctrlchar");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();

    // unbound Ctrl/Super letters are swallowed, not typed
    app.handle_key(ctrl('k'));
    app.handle_key(cmd('k'));
    assert_eq!(app.buffer.lines, vec!["alpha"]);

    // bound shortcuts don't type their letter either
    app.handle_key(ctrl('c'));
    assert_eq!(app.buffer.lines, vec!["alpha"]);
    assert_eq!(app.buffer.cursor, (0, 0));
}

#[test]
fn ctrl_q_requires_second_press_when_dirty() {
    let dir = scratch("quit");
    let mut app = new_app(dir, None).unwrap();
    app.handle_key(ctrl('o'));
    app.handle_key(char_key('x'));
    app.handle_key(ctrl('q'));
    assert!(!app.should_quit); // blocked, message shown
    app.handle_key(ctrl('q'));
    assert!(app.should_quit);
}

#[test]
fn ctrl_q_quits_immediately_when_clean() {
    let dir = scratch("quit2");
    let mut app = new_app(dir, None).unwrap();
    app.handle_key(ctrl('q'));
    assert!(app.should_quit);
}

#[test]
fn ctrl_r_refreshes_the_sidebar_after_external_changes() {
    let dir = scratch("reload6");
    fs::write(dir.join("a.txt"), "x").unwrap();
    fs::write(dir.join("gone.txt"), "x").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    assert!(app.sidebar.entries.iter().any(|e| e.name == "gone.txt"));

    fs::remove_file(dir.join("gone.txt")).unwrap();
    fs::write(dir.join("fresh.txt"), "y").unwrap();
    app.handle_key(ctrl('r'));

    let names: Vec<&str> = app
        .sidebar
        .entries
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert!(names.contains(&"fresh.txt"));
    assert!(!names.contains(&"gone.txt"));
    // the open file itself is unaffected
    assert_eq!(app.buffer.lines, vec!["x"]);
}

#[test]
fn enter_auto_indents() {
    let dir = scratch("autoindent");
    fs::write(dir.join("a.txt"), "").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    for c in "    fn f() {}".chars() {
        app.handle_key(char_key(c));
    }
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.buffer.lines, vec!["    fn f() {}", "    "]);
    assert_eq!(app.buffer.cursor, (4, 1));
}

#[test]
fn tab_indents_selection_and_shift_tab_dedents() {
    let dir = scratch("tabindent");
    fs::write(dir.join("a.txt"), "").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    app.buffer.insert_multiline("    a\n    b");
    app.buffer.home();
    app.buffer.move_up(); // top-left
    app.buffer.begin_selection();
    app.buffer.move_down();
    app.buffer.end();

    // Tab with a selection block-indents instead of replacing it
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.buffer.lines, vec!["        a", "        b"]);

    // Shift+Tab (both the BackTab and the Tab+SHIFT encodings) dedents
    app.handle_key(key(KeyCode::BackTab));
    assert_eq!(app.buffer.lines, vec!["    a", "    b"]);
    app.handle_key(shift_key(KeyCode::Tab));
    assert_eq!(app.buffer.lines, vec!["a", "b"]);
}

#[test]
fn sidebar_backspace_ascends() {
    let dir = scratch("ascend");
    fs::create_dir(dir.join("sub")).unwrap();
    let mut app = new_app(dir.clone(), None).unwrap();
    app.sidebar.select_name("sub");
    app.handle_key(key(KeyCode::Enter)); // descend
    assert_eq!(app.sidebar.dir, dir.join("sub"));
    app.handle_key(key(KeyCode::Backspace)); // ascend
    assert_eq!(app.sidebar.dir, dir);
}

#[test]
fn sidebar_arrow_keys_open_and_ascend() {
    let dir = scratch("arrows");
    fs::create_dir(dir.join("sub")).unwrap();
    fs::write(dir.join("sub").join("f.txt"), "x").unwrap();
    let mut app = new_app(dir.clone(), None).unwrap();

    // → enters the selected directory, ← goes back up
    app.sidebar.select_name("sub");
    app.handle_key(key(KeyCode::Right)); // descend
    assert_eq!(app.sidebar.dir, dir.join("sub"));
    app.handle_key(key(KeyCode::Left)); // ascend
    assert_eq!(app.sidebar.dir, dir);

    // → on a file opens it in the editor (focus moves)
    app.sidebar.select_name("sub");
    app.handle_key(key(KeyCode::Right));
    app.sidebar.select_name("f.txt");
    app.handle_key(key(KeyCode::Right));
    assert_eq!(app.buffer.path, Some(dir.join("sub").join("f.txt")));
    assert_eq!(app.focus, Focus::Editor);
}

#[test]
fn uses_catppuccin_mocha_theme() {
    assert_eq!(THEME.name, ThemeName::CatppuccinMocha);
    assert_eq!(PALETTE.bg, Color::Rgb(30, 30, 46));
    assert_eq!(PALETTE.accent, Color::Rgb(137, 180, 250));
    assert_eq!(PALETTE.selection, Color::Rgb(49, 50, 68));
}

#[test]
fn focused_panels_use_theme_accent() {
    let dir = scratch("focus-color");
    let file = dir.join("notes.txt");
    fs::write(&file, "hello\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();

    let buf = render_buffer(&mut app);
    assert_eq!(
        buf.cell((app.editor_area.x, app.editor_area.y + 1))
            .unwrap()
            .style()
            .fg,
        Some(FOCUS_COLOR)
    );

    app.focus = Focus::Sidebar;
    let buf = render_buffer(&mut app);
    assert_eq!(
        buf.cell((app.sidebar_area.x, app.sidebar_area.y + 1))
            .unwrap()
            .style()
            .fg,
        Some(FOCUS_COLOR)
    );
}

#[test]
fn focused_sidebar_hides_native_cursor() {
    let dir = scratch("sidebar-cursor");
    fs::write(dir.join("notes.txt"), "hello world").unwrap();
    let mut app = new_app(dir, None).unwrap();
    let backend = TestBackend::new(150, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|f| app.draw(f)).unwrap();

    assert!(!terminal.backend().cursor_visible());
}

#[test]
fn editing_rehighlights_immediately() {
    let dir = scratch("hlrehighlight");
    let file = dir.join("code.rs");
    fs::write(&file, "fn main() {\n}\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    let buf = render_buffer(&mut app);
    assert_ne!(buf.cell((31, 2)).unwrap().style().fg, Some(Color::Reset));
    // typing 'x' in front of "fn" must immediately re-highlight:
    // "xfn" is no longer a keyword
    app.handle_key(char_key('x'));
    let buf = render_buffer(&mut app);
    assert_eq!(buf.cell((31, 3)).unwrap().symbol(), "x");
    assert_eq!(buf.cell((31, 3)).unwrap().style().fg, Some(Color::Reset));
    assert_eq!(buf.cell((32, 3)).unwrap().symbol(), "f");
}

#[test]
fn status_bar_shows_detected_syntax() {
    let dir = scratch("hlsyntax");
    let file = dir.join("code.rs");
    fs::write(&file, "fn main() {}\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    let rows = render(&mut app);
    assert!(row_contains(&rows, "[Rust] ○ saved"));

    let dir = scratch("hlsyntax2");
    let file = dir.join("notes.txt");
    fs::write(&file, "hello\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    let rows = render(&mut app);
    assert!(!row_contains(&rows, "[Plain Text]"));

    for (name, body) in [("ui.cljs", "(defn hi [] 1)\n"), ("config.edn", "{:a 1}\n")] {
        let dir = scratch(&format!("hlsyntax-{name}"));
        let file = dir.join(name);
        fs::write(&file, body).unwrap();
        let mut app = new_app(dir, Some(file)).unwrap();
        let rows = render(&mut app);
        assert!(
            row_contains(&rows, "[Clojure] ○ saved"),
            "{name} should show Clojure in the status bar"
        );
    }
}

#[test]
fn partial_selection_splits_highlighted_span() {
    let line = "fn main";
    let syntax_style = Style::default().fg(Color::Blue);
    let spans = clip_ops(
        line,
        &[(Some(syntax_style), 0..line.len())],
        0,
        line.chars().count(),
        Some((3, 5)),
        &[],
        ColorSupport::TrueColor,
    );

    assert_eq!(
        spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>(),
        vec!["fn ", "ma", "in"]
    );
    assert_eq!(spans[0].style.bg, None);
    assert_eq!(spans[1].style.bg, Some(PALETTE.selection));
    assert_eq!(spans[2].style.bg, None);
    assert_eq!(spans[0].style.fg, Some(Color::Blue));
    assert_eq!(spans[1].style.fg, Some(Color::Blue));
    assert_eq!(spans[2].style.fg, Some(Color::Blue));
}

#[test]
fn selection_byte_range_is_not_reinterpreted_as_char_range() {
    let line = "— browse directories on the left";
    let start = line.find("directories").unwrap();
    let end = start + "directories".len();
    let spans = clip_ops(
        line,
        &[(None, 0..line.len())],
        0,
        line.chars().count(),
        Some((start, end)),
        &[],
        ColorSupport::TrueColor,
    );

    let selected: Vec<&str> = spans
        .iter()
        .filter(|span| span.style.bg == Some(PALETTE.selection))
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(selected, vec!["directories"]);
}
