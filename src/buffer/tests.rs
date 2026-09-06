//! Shared fixtures for the behavior regression suite.

use super::*;

fn empty() -> Buffer {
    Buffer::empty()
}

/// Scratch area inside the workspace (the sandbox blocks /tmp writes).
fn tmp_path(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn typed(b: &mut Buffer, text: &str) {
    b.insert_text(text);
}

fn clj_buf() -> Buffer {
    Buffer::empty_at(PathBuf::from("test.clj"))
}

fn type_clj(b: &mut Buffer, text: &str) {
    for c in text.chars() {
        if c == '\n' {
            b.newline();
        } else {
            b.insert_char(c);
        }
        b.apply_parinfer();
    }
}

const CJK: &str = "界";
const COMBINING: &str = "e\u{0301}";
const ZWJ: &str = "👩\u{200D}💻";

fn assert_cursor_in_range(b: &Buffer) {
    assert!(b.cursor.1 < b.lines.len());
    assert!(b.cursor.0 <= b.lines[b.cursor.1].chars().count());
}

mod editing;
mod history;
mod navigation;
mod parinfer;
mod selection;
mod storage;
mod wrapping;
