use super::*;

#[test]
fn typing_then_backspacing_round_trips() {
    let mut b = empty();
    b.insert_text("hello");
    assert_eq!(b.lines, vec!["hello"]);
    assert_eq!(b.cursor, (5, 0));
    assert!(b.dirty);
    for _ in 0..5 {
        b.backspace();
    }
    assert_eq!(b.lines, vec![""]);
    assert_eq!(b.cursor, (0, 0));
}

#[test]
fn toggle_wrap_round_trips_and_maps_scroll() {
    let mut b = empty();
    typed(&mut b, "abcdefghij"); // 2 rows at width 5
    b.newline();
    typed(&mut b, "xyz");
    b.toggle_wrap(5);
    assert!(b.wrap);
    assert_eq!(b.scroll.0, 0);
    // scroll to the last visual row (line 1), then leave wrap mode:
    // the viewport maps back to the logical line it starts on
    b.scroll.1 = 2;
    b.toggle_wrap(5);
    assert!(!b.wrap);
    assert_eq!(b.scroll.1, 1);
    b.toggle_wrap(5);
    assert!(b.wrap);
    assert_eq!(b.scroll.0, 0);
}

#[test]
fn save_load_round_trip_with_trailing_newline() {
    let path = tmp_path("with-nl.txt");
    std::fs::write(&path, "one\ntwo\n").unwrap();

    let loaded = Buffer::from_path(path.clone()).unwrap();
    assert_eq!(loaded.lines, vec!["one", "two", ""]);
    // saving the loaded buffer reproduces the file exactly
    let mut loaded = loaded;
    loaded.save().unwrap();
    assert!(!loaded.dirty);
    let disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(disk, "one\ntwo\n");

    // deleting the trailing empty line removes the trailing newline
    loaded.home();
    loaded.move_down();
    loaded.move_down();
    loaded.backspace();
    loaded.save().unwrap();
    let disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(disk, "one\ntwo");
}

#[test]
fn save_load_round_trip_without_trailing_newline() {
    let path = tmp_path("no-nl.txt");
    let mut b = empty();
    b.insert_text("one");
    b.newline();
    b.insert_text("two");
    b.path = Some(path.clone());
    b.save().unwrap();
    let disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(disk, "one\ntwo");

    let loaded = Buffer::from_path(path).unwrap();
    assert_eq!(loaded.lines, vec!["one", "two"]);
}

#[test]
fn empty_file_loads_as_single_empty_line() {
    let path = tmp_path("empty.txt");
    std::fs::write(&path, "").unwrap();
    let loaded = Buffer::from_path(path).unwrap();
    assert_eq!(loaded.lines, vec![""]);
}

#[test]
fn empty_at_does_not_create_the_file_until_save() {
    let path = tmp_path("new-on-save.txt");
    let _ = std::fs::remove_file(&path);
    let mut b = Buffer::empty_at(path.clone());
    assert_eq!(b.lines, vec![""]);
    assert_eq!(b.path.as_ref(), Some(&path));
    assert!(!b.dirty);
    assert!(!path.exists());

    b.insert_text("hi");
    b.save().unwrap();
    assert!(!b.dirty);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi");
}

#[test]
fn crlf_files_are_not_corrupted() {
    let path = tmp_path("crlf.txt");
    std::fs::write(&path, "a\r\nb\r\n").unwrap();
    let loaded = Buffer::from_path(path.clone()).unwrap();
    assert_eq!(loaded.lines, vec!["a", "b", ""]);
    let mut loaded = loaded;
    loaded.save().unwrap();
    let disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(disk, "a\r\nb\r\n");
}

#[test]
fn save_without_path_errors() {
    let mut b = empty();
    assert!(b.save().is_err());
}

#[test]
fn undo_redo_round_trip_restores_text_cursor_and_dirty() {
    let mut b = empty();
    b.insert_text("hello");
    assert!(b.dirty);
    b.undo();
    assert_eq!(b.lines, vec![""]);
    assert_eq!(b.cursor, (0, 0));
    assert!(!b.dirty); // back to the pristine state
    b.redo();
    assert_eq!(b.lines, vec!["hello"]);
    assert_eq!(b.cursor, (5, 0));
    assert!(b.dirty);
    b.undo();
    assert_eq!(b.lines, vec![""]);
    assert!(!b.dirty);
}

#[test]
fn save_breaks_the_typing_chain() {
    let path = tmp_path("undo-save.txt");
    let mut b = empty();
    b.path = Some(path.clone());
    b.insert_text("ab");
    b.save().unwrap();
    b.insert_text("c");
    // undoing the post-save typing stops at the saved text
    b.undo();
    assert_eq!(b.lines, vec!["ab"]);
    assert!(!b.dirty);
    b.undo();
    assert_eq!(b.lines, vec![""]);
}

#[test]
fn atomic_save_failure_retains_original_and_cleans_staging_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    fs::write(&path, "original").unwrap();
    let result = atomic_save(&path, true, |file| {
        file.write_all(b"partial")?;
        Err(io::Error::other("injected disk failure"))
    });
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn atomic_save_never_clobbers_a_destination_created_during_staging() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let result = atomic_save(&path, false, |file| {
        file.write_all(b"new")?;
        fs::write(&path, "other writer")
    });
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "other writer");
}

#[cfg(unix)]
#[test]
fn atomic_save_preserves_symlink_and_target_permissions() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let link = dir.path().join("link");
    fs::write(&target, "old").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    symlink("target", &link).unwrap();
    let mut buffer = Buffer::from_path(link.clone()).unwrap();
    buffer.insert_text("new");
    buffer.save().unwrap();
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "newold");
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn crlf_edit_newline_paste_undo_and_save_keep_separators_out_of_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("windows.txt");
    fs::write(&path, "abc\r\ndef\r\n").unwrap();
    let mut buffer = Buffer::from_path(path.clone()).unwrap();
    buffer.save().unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "abc\r\ndef\r\n");
    buffer.end();
    buffer.insert_char('X');
    buffer.save().unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "abcX\r\ndef\r\n");
    buffer.newline();
    buffer.insert_multiline("p\r\nq");
    assert!(buffer.lines.iter().all(|line| !line.contains('\r')));
    buffer.undo();
    buffer.undo();
    buffer.save().unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "abcX\r\ndef\r\n");
}

#[test]
fn readonly_destination_is_unchanged_and_buffer_stays_dirty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("readonly.txt");
    fs::write(&path, "original").unwrap();
    let writable = fs::metadata(&path).unwrap().permissions();
    let mut readonly = writable.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&path, readonly).unwrap();
    let mut buffer = Buffer::from_path(path.clone()).unwrap();
    buffer.insert_char('X');
    let result = buffer.save();
    // Restore permissions before assertions so cleanup also works on Windows.
    fs::set_permissions(&path, writable).unwrap();
    assert!(result.is_err());
    assert!(buffer.dirty);
    assert_eq!(fs::read_to_string(path).unwrap(), "original");
}

#[test]
fn unedited_mixed_endings_round_trip_and_edits_use_crlf() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mixed.txt");
    fs::write(&path, "a\r\nb\nc").unwrap();
    let mut buffer = Buffer::from_path(path.clone()).unwrap();
    buffer.save().unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "a\r\nb\nc");
    buffer.end();
    buffer.insert_char('X');
    buffer.save().unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "aX\r\nb\r\nc");
}
