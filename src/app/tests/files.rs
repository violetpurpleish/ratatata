use super::*;

#[test]
fn opening_a_file_arg_focuses_editor() {
    let dir = scratch("filearg");
    let file = dir.join("a.txt");
    fs::write(&file, "hello\nworld\n").unwrap();
    let app = new_app(dir, Some(file)).unwrap();
    assert_eq!(app.focus, Focus::Editor);
    assert_eq!(app.buffer.lines, vec!["hello", "world", ""]);
    assert!(!app.buffer.dirty);
}

#[test]
fn missing_file_arg_opens_an_empty_buffer_bound_to_that_path() {
    let dir = scratch("missingarg");
    let file = dir.join("somefile.md");
    assert!(!file.exists());
    let mut app = new_app(dir, Some(file.clone())).unwrap();
    assert_eq!(app.focus, Focus::Editor);
    assert_eq!(app.buffer.lines, vec![""]);
    assert_eq!(app.buffer.path.as_ref(), Some(&file));
    assert!(!app.buffer.dirty);
    assert!(app.image.is_none());
    assert!(!file.exists(), "the file must not be created until save");

    let rows = render(&mut app);
    assert!(row_contains(&rows, "somefile.md"));
    assert!(!row_contains(&rows, "untitled"));

    // Ctrl+S writes the bound path without a save-as prompt
    app.handle_key(char_key('x'));
    assert!(app.buffer.dirty);
    app.handle_key(ctrl('s'));
    assert!(app.save_as_input.is_none());
    assert!(!app.buffer.dirty);
    assert_eq!(fs::read_to_string(&file).unwrap(), "x");
}

#[test]
fn directory_arg_focuses_sidebar() {
    let dir = scratch("dira");
    let app = new_app(dir, None).unwrap();
    assert_eq!(app.focus, Focus::Sidebar);
    assert!(app.buffer.path.is_none());
}

#[test]
fn save_as_flow_creates_file_and_updates_sidebar() {
    let dir = scratch("saveas");
    let mut app = new_app(dir.clone(), None).unwrap();

    app.handle_key(ctrl('o')); // focus editor
    for c in "hello".chars() {
        app.handle_key(char_key(c));
    }
    assert!(app.buffer.dirty);

    // Ctrl+S with no file name enters the save-as prompt
    app.handle_key(ctrl('s'));
    assert!(app.save_as_input.is_some());

    // typing while the prompt is active goes into the prompt, not the buffer
    let save_path = dir.join("out.txt");
    for c in save_path.display().to_string().chars() {
        app.handle_key(char_key(c));
    }
    assert_eq!(
        app.save_as_input.as_deref(),
        Some(save_path.to_str().unwrap())
    );
    assert_eq!(app.buffer.lines, vec!["hello"]);

    app.handle_key(key(KeyCode::Enter));
    assert!(app.save_as_input.is_none());
    assert!(!app.buffer.dirty);
    assert_eq!(fs::read_to_string(&save_path).unwrap(), "hello");
    // the sidebar was refreshed and now lists the new file
    assert!(app.sidebar.entries.iter().any(|e| e.name == "out.txt"));
}

#[test]
fn escape_cancels_save_as() {
    let dir = scratch("cancel");
    let mut app = new_app(dir, None).unwrap();
    app.handle_key(ctrl('o'));
    app.handle_key(ctrl('s'));
    app.handle_key(char_key('x'));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.save_as_input.is_none());
    assert!(app.buffer.path.is_none());
}

#[test]
fn ctrl_n_opens_a_new_untitled_buffer() {
    let dir = scratch("newn");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    assert_eq!(app.buffer.lines, vec!["alpha"]);
    assert!(app.buffer.path.is_some());

    app.handle_key(ctrl('n'));
    assert_eq!(app.buffer.lines, vec![""]);
    assert!(app.buffer.path.is_none());
    assert!(!app.buffer.dirty);
    assert!(app.image.is_none());
    // focus stays on the editor so the new file can be typed into
    assert_eq!(app.focus, Focus::Editor);
    assert!(app.message.is_some());

    // typing lands in the new buffer
    app.handle_key(char_key('x'));
    assert_eq!(app.buffer.lines, vec!["x"]);
}

#[test]
fn ctrl_n_from_a_clean_untitled_buffer_resets_it() {
    let dir = scratch("newn2");
    let mut app = new_app(dir, None).unwrap();
    app.handle_key(ctrl('o'));
    app.handle_key(char_key('x'));
    app.handle_key(ctrl('z')); // undo -> clean again
    assert!(!app.buffer.dirty);

    app.handle_key(ctrl('n'));
    assert_eq!(app.buffer.lines, vec![""]);
    assert!(!app.buffer.dirty);
    assert_eq!(app.focus, Focus::Editor);
}

#[test]
fn ctrl_n_refuses_when_the_buffer_is_dirty() {
    let dir = scratch("newn3");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    app.handle_key(char_key('X'));
    assert!(app.buffer.dirty);

    app.handle_key(ctrl('n'));
    assert_eq!(app.buffer.lines, vec!["Xalpha"]); // untouched
    assert_eq!(
        app.buffer.path.as_ref().unwrap().file_name().unwrap(),
        "a.txt"
    );
    assert!(app.message.is_some());
}

#[test]
fn ctrl_n_in_the_save_as_prompt_does_nothing() {
    let dir = scratch("newn5");
    let mut app = new_app(dir, None).unwrap();
    app.handle_key(ctrl('o'));
    app.handle_key(char_key('x'));
    app.handle_key(ctrl('s'));
    assert!(app.save_as_input.is_some());
    app.handle_key(char_key('a'));

    app.handle_key(ctrl('n'));
    // the prompt and its text survive, the buffer is untouched
    assert!(app.save_as_input.is_some());
    assert_eq!(app.save_as_input.as_deref(), Some("a"));
    assert_eq!(app.buffer.lines, vec!["x"]);
}

#[test]
fn save_as_prompt_accepts_alt_and_shift_chars() {
    let dir = scratch("saveasmod");
    let mut app = new_app(dir, None).unwrap();
    app.handle_key(ctrl('o'));
    app.handle_key(ctrl('s'));
    assert!(app.save_as_input.is_some());

    // `[` needs Option on German layouts; `:` and `a` need Shift on many
    // layouts (the `a` is folded to uppercase)
    app.handle_key(alt('['));
    app.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SHIFT));
    assert_eq!(app.save_as_input.as_deref(), Some("[:A"));
    assert_eq!(app.buffer.lines, vec![""]);
}

#[test]
fn sidebar_enter_opens_file_and_switches_focus() {
    let dir = scratch("open");
    fs::write(dir.join("b.txt"), "beta").unwrap();
    let mut app = new_app(dir, None).unwrap();
    app.sidebar.select_name("b.txt");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.focus, Focus::Editor);
    assert_eq!(app.buffer.lines, vec!["beta"]);
    assert_eq!(
        app.buffer.path.as_ref().unwrap().file_name().unwrap(),
        "b.txt"
    );
}

#[test]
fn dirty_buffer_blocks_opening_another_file() {
    let dir = scratch("dirtyblock");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    fs::write(dir.join("b.txt"), "beta").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    app.handle_key(char_key('X')); // make dirty
    assert!(app.buffer.dirty);

    app.sidebar.select_name("b.txt");
    app.handle_key(ctrl('o')); // sidebar
    app.handle_key(key(KeyCode::Enter));

    // still on a.txt with the edit intact (cursor starts at 0,0)
    assert_eq!(app.buffer.lines, vec!["Xalpha"]);
    assert_eq!(
        app.buffer.path.as_ref().unwrap().file_name().unwrap(),
        "a.txt"
    );
    assert!(app.message.is_some());
}

#[test]
fn ctrl_s_saves_and_clears_dirty() {
    let dir = scratch("save");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(file)).unwrap();
    app.handle_key(char_key('Z'));
    assert!(app.buffer.dirty);
    app.handle_key(ctrl('s'));
    assert!(!app.buffer.dirty);
    assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "Zalpha");
}

#[test]
fn ctrl_r_reloads_the_open_file_from_disk() {
    let dir = scratch("reload1");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha\n").unwrap();
    let mut app = new_app(dir.clone(), Some(file.clone())).unwrap();
    assert_eq!(app.buffer.lines, vec!["alpha", ""]);
    app.buffer.cursor = (2, 0);
    app.handle_key(ctrl('w')); // wrapping survives the reload
    assert!(app.buffer.wrap);

    // the file changes on disk (e.g. by another program)
    fs::write(&file, "beta\ngamma\n").unwrap();
    app.handle_key(ctrl('r'));
    assert_eq!(app.buffer.lines, vec!["beta", "gamma", ""]);
    assert!(!app.buffer.dirty);
    assert_eq!(app.buffer.path, Some(file));
    assert!(app.buffer.wrap, "wrapping survives a reload");
    assert_eq!(app.buffer.cursor, (2, 0));
    assert!(app.message.is_some());
}

#[test]
fn cmd_r_reloads_like_ctrl_r() {
    let dir = scratch("reloadcmd");
    let file = dir.join("a.txt");
    fs::write(&file, "one").unwrap();
    let mut app = new_app(dir.clone(), Some(file)).unwrap();
    fs::write(dir.join("a.txt"), "two").unwrap();
    app.handle_key(cmd('r'));
    assert_eq!(app.buffer.lines, vec!["two"]);
}

#[test]
fn ctrl_r_refuses_unsaved_changes_but_still_refreshes_the_sidebar() {
    let dir = scratch("reload2");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha\n").unwrap();
    let mut app = new_app(dir.clone(), Some(file.clone())).unwrap();
    app.handle_key(char_key('X'));
    assert!(app.buffer.dirty);

    fs::write(&file, "changed on disk\n").unwrap();
    fs::write(dir.join("added.txt"), "new\n").unwrap();
    app.handle_key(ctrl('r'));
    // the in-memory edit survives; the disk change is not loaded
    assert_eq!(app.buffer.lines, vec!["Xalpha", ""]);
    assert_eq!(
        app.buffer.path.as_ref().unwrap().file_name().unwrap(),
        "a.txt"
    );
    // the sidebar still reflects the external changes
    assert!(app.sidebar.entries.iter().any(|e| e.name == "added.txt"));
    assert!(app.message.is_some());
}

#[test]
fn ctrl_r_without_a_file_shows_a_message() {
    let dir = scratch("reload3");
    let mut app = new_app(dir, None).unwrap();
    app.handle_key(ctrl('r'));
    assert_eq!(app.buffer.lines, vec![""]);
    assert!(app.message.is_some());
}

#[test]
fn ctrl_r_reload_failure_keeps_the_buffer_and_refreshes_the_sidebar() {
    let dir = scratch("reload5");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha\n").unwrap();
    let mut app = new_app(dir.clone(), Some(file.clone())).unwrap();
    fs::remove_file(&file).unwrap();

    app.handle_key(ctrl('r'));
    // the failed reload leaves the buffer as it was...
    assert_eq!(app.buffer.lines, vec!["alpha", ""]);
    assert!(!app.buffer.dirty);
    // ...but the sidebar shows the file is gone
    assert!(!app.sidebar.entries.iter().any(|e| e.name == "a.txt"));
    assert!(app.message.is_some());
}

#[test]
fn renders_file_content_and_modified_state() {
    let dir = scratch("render2");
    let file = dir.join("notes.txt");
    fs::write(&file, "hello world").unwrap();
    let mut app = new_app(dir.clone(), Some(file)).unwrap();

    // clean state shows the content and the saved marker
    let rows = render(&mut app);
    assert!(row_contains(&rows, "hello world"));
    assert!(row_contains(&rows, "EDITOR"));
    assert!(row_contains(&rows, "○ saved"));

    // type something -> modified marker appears
    app.handle_key(char_key('!'));
    let rows = render(&mut app);
    assert!(row_contains(&rows, "● modified"));

    // save -> message shown, and the marker returns once it expires
    app.handle_key(ctrl('s'));
    let rows = render(&mut app);
    assert!(row_contains(&rows, "saved "));
    app.message = None; // simulate the message expiring
    let rows = render(&mut app);
    assert!(row_contains(&rows, "○ saved"));
}

#[test]
fn plain_text_files_render_uncolored() {
    let dir = scratch("hlplain");
    let file = dir.join("notes.txt");
    fs::write(&file, "just some words\n").unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    let buf = render_buffer(&mut app);
    for x in 31..99 {
        // exclude the accent-colored focus border at x=99
        let cell = buf.cell((x, 3)).unwrap();
        if cell.symbol().is_empty() || cell.symbol() == " " {
            continue;
        }
        assert_eq!(cell.style().fg, Some(Color::Reset), "col {x}");
    }
}

#[test]
fn renders_tab_indentation_in_plain_text_files() {
    let dir = scratch("tabindent-render");
    let file = dir.join("actions.coffee");
    fs::write(
        &file,
        "GLOBALS.actions =\n\t\"conversation\": (focused) ->\n",
    )
    .unwrap();
    let mut app = new_app(dir, Some(file)).unwrap();
    let buf = render_buffer(&mut app);

    // Ratatui skips literal tab control characters. The editor keeps the
    // tab in its buffer, but expands it for display so unsupported files
    // retain their indentation.
    for x in 31..35 {
        assert_eq!(buf.cell((x, 4)).unwrap().symbol(), " ", "column {x}");
    }
    assert_eq!(buf.cell((35, 4)).unwrap().symbol(), "\"");

    // The logical cursor is still before the tab. Its one-cell caret
    // must use a printable placeholder too, rather than a raw tab.
    app.buffer.cursor = (0, 1);
    let buf = render_buffer(&mut app);
    assert_eq!(buf.cell((31, 4)).unwrap().symbol(), " ");
    assert_eq!(buf.cell((31, 4)).unwrap().style().bg, Some(PALETTE.warning));
}

#[test]
fn renders_save_as_prompt() {
    let dir = scratch("render3");
    let mut app = new_app(dir, None).unwrap();
    app.handle_key(ctrl('o'));
    app.handle_key(char_key('x'));
    app.handle_key(ctrl('s'));
    let rows = render(&mut app);
    assert!(row_contains(&rows, "save as:"));
}

#[test]
fn sidebar_double_click_on_directory_descends() {
    let dir = scratch("mdblclickdir");
    fs::create_dir(dir.join("sub")).unwrap();
    let mut app = new_app(dir.clone(), None).unwrap();
    render_buffer(&mut app);

    let click = |kind| mouse(kind, 5, 3); // row 0: ".."
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left)));
    app.handle_mouse(click(MouseEventKind::Up(MouseButton::Left)));
    // opened the parent of the scratch dir
    assert_ne!(app.sidebar.dir, dir);
}

#[test]
fn clicking_save_button_saves_the_file() {
    let dir = scratch("mbtnsave");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(file)).unwrap();
    render_buffer(&mut app); // builds the button rectangles

    app.handle_key(char_key('X'));
    assert!(app.buffer.dirty);
    click_button(&mut app, Shortcut::Save);
    assert!(!app.buffer.dirty);
    assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "Xalpha");
}

#[test]
fn clicking_files_button_toggles_the_sidebar() {
    let dir = scratch("mbtnfiles");
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(dir.join("a.txt"))).unwrap();
    render_buffer(&mut app);
    let shown_width = app.editor_area.width;
    assert!(app.sidebar_visible);
    assert!(app.sidebar_area.width > 0);

    click_button(&mut app, Shortcut::ToggleSidebar);
    assert!(!app.sidebar_visible);
    render_buffer(&mut app);
    assert_eq!(app.sidebar_area.width, 0);
    assert!(app.editor_area.width > shown_width);
    assert_eq!(app.focus, Focus::Editor);

    click_button(&mut app, Shortcut::ToggleSidebar);
    assert!(app.sidebar_visible);
    render_buffer(&mut app);
    assert_eq!(app.editor_area.width, shown_width);
}

#[test]
fn ctrl_z_in_save_as_prompt_leaves_buffer_alone() {
    let dir = scratch("undoredo5");
    let mut app = new_app(dir, None).unwrap();
    app.handle_key(ctrl('o'));
    app.handle_key(char_key('x'));
    app.handle_key(ctrl('s')); // no file name yet: save-as prompt
    assert!(app.save_as_input.is_some());

    app.handle_key(ctrl('z'));
    assert!(app.save_as_input.is_some());
    assert_eq!(app.buffer.lines, vec!["x"]);
}

#[test]
fn undo_redo_round_trip_across_save() {
    let dir = scratch("undoredo6");
    let file = dir.join("a.txt");
    fs::write(&file, "alpha").unwrap();
    let mut app = new_app(dir.clone(), Some(file)).unwrap();

    app.handle_key(char_key('X'));
    app.handle_key(ctrl('s')); // save: "Xalpha" on disk
    assert!(!app.buffer.dirty);

    // undoing past the save restores the pre-edit text; the buffer
    // now differs from what is on disk, so it is dirty again
    app.handle_key(ctrl('z'));
    assert_eq!(app.buffer.lines, vec!["alpha"]);
    assert!(app.buffer.dirty);
    // redoing brings back the saved content, which matches the disk
    app.handle_key(ctrl_shift('z'));
    assert_eq!(app.buffer.lines, vec!["Xalpha"]);
    assert!(!app.buffer.dirty);
    assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "Xalpha");
}

#[test]
fn ctrl_h_toggles_dotfiles_in_the_sidebar() {
    let dir = scratch("apphidden");
    fs::write(dir.join(".env"), "SECRET=1").unwrap();
    fs::write(dir.join("shown.txt"), "ok").unwrap();
    let mut app = new_app(dir, None).unwrap();
    let names = |app: &App| {
        app.sidebar
            .entries
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>()
    };
    assert!(app.sidebar.hide_dotfiles);
    assert_eq!(names(&app), vec!["..".to_string(), "shown.txt".to_string()]);
    app.handle_key(ctrl('h'));
    assert!(!app.sidebar.hide_dotfiles);
    assert_eq!(
        names(&app),
        vec![
            "..".to_string(),
            ".env".to_string(),
            "shown.txt".to_string()
        ]
    );
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(msg, _)| msg.contains("dotfiles shown"))
    );
    app.handle_key(ctrl('h'));
    assert!(app.sidebar.hide_dotfiles);
    assert_eq!(names(&app), vec!["..".to_string(), "shown.txt".to_string()]);
    let rows = render(&mut app);
    assert!(row_contains(&rows, "Ctrl+H hidden"));
}

#[test]
fn save_as_requires_confirmation_and_cancel_preserves_original() {
    let dir = scratch("release-overwrite");
    let path = dir.join("existing.txt");
    fs::write(&path, "original").unwrap();
    let mut app = new_app(dir, None).unwrap();
    app.buffer.insert_text("replacement");
    app.save_as_input = Some(path.to_str().unwrap().into());
    app.confirm_save_as();
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    assert!(app.buffer.path.is_none());
    let rendered = render_buffer(&mut app);
    assert!(format!("{rendered:?}").contains("exists"));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.save_as_input.is_none());
    app.save();
    app.save_as_input = Some(path.to_str().unwrap().into());
    app.confirm_save_as();
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    app.confirm_save_as();
    assert_eq!(fs::read_to_string(&path).unwrap(), "replacement");
    assert!(!app.buffer.dirty);
}

#[test]
fn failed_save_as_can_be_corrected_without_losing_edits() {
    let dir = scratch("release-retry");
    let mut app = new_app(dir.clone(), None).unwrap();
    app.buffer.insert_text("work");
    app.save_as_input = Some(dir.join("missing/out.txt").to_str().unwrap().into());
    app.confirm_save_as();
    assert!(app.buffer.path.is_none());
    assert!(app.save_as_input.is_some());
    assert!(app.save_as_error.is_some());
    assert!(app.buffer.dirty);
    let corrected = dir.join("out.txt");
    app.save_as_input = Some(corrected.to_str().unwrap().into());
    app.confirm_save_as();
    assert_eq!(fs::read_to_string(corrected).unwrap(), "work");
    assert!(app.save_as_input.is_none());
}

#[cfg(unix)]
#[test]
fn sidebar_opens_directory_symlink() {
    let dir = scratch("release-dirlink");
    fs::create_dir(dir.join("real")).unwrap();
    fs::write(dir.join("real/inside.txt"), "text").unwrap();
    std::os::unix::fs::symlink("real", dir.join("link")).unwrap();
    let mut app = new_app(dir, None).unwrap();
    app.sidebar.select_name("link");
    assert!(app.sidebar.selected_is_dir());
    app.open_selected();
    assert!(app.sidebar.entries.iter().any(|e| e.name == "inside.txt"));
}
