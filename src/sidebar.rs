//! Directory sidebar: a scrollable listing of the current directory.

use std::fs;
use std::io;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The ".." entry (go up one level).
    Parent,
    Dir,
    File,
}

pub struct Entry {
    pub name: String,
    pub kind: Kind,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.kind != Kind::File
    }

    pub fn is_hidden(&self) -> bool {
        self.kind != Kind::Parent && self.name.starts_with('.')
    }
}

pub struct Sidebar {
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub scroll: usize,
    /// When true (the default), entries whose names start with `.` are
    /// omitted from the listing. The parent entry (`..`) is never hidden.
    pub hide_dotfiles: bool,
}

impl Sidebar {
    pub fn new(dir: PathBuf) -> io::Result<Self> {
        let mut s = Self {
            dir,
            entries: Vec::new(),
            selected: 0,
            scroll: 0,
            hide_dotfiles: true,
        };
        s.reload()?;
        Ok(s)
    }

    /// Re-read the directory. Sort order: ".." first, then directories,
    /// then files, each group alphabetically (case-insensitive).
    pub fn reload(&mut self) -> io::Result<()> {
        let mut entries: Vec<Entry> = Vec::new();
        if self.dir.parent().is_some_and(|p| !p.as_os_str().is_empty()) {
            entries.push(Entry {
                name: "..".into(),
                kind: Kind::Parent,
            });
        }

        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if self.hide_dotfiles && name.starts_with('.') {
                continue;
            }
            // follow symlinks so symlinked dirs show up as dirs
            let is_dir = entry.metadata().map(|m| m.is_dir()).unwrap_or(false);
            if is_dir {
                dirs.push(name);
            } else {
                files.push(name);
            }
        }
        let cmp = |a: &String, b: &String| a.to_lowercase().cmp(&b.to_lowercase());
        dirs.sort_by(cmp);
        files.sort_by(cmp);
        entries.extend(dirs.into_iter().map(|name| Entry {
            name,
            kind: Kind::Dir,
        }));
        entries.extend(files.into_iter().map(|name| Entry {
            name,
            kind: Kind::File,
        }));

        self.entries = entries;
        self.clamp_selection();
        Ok(())
    }

    fn clamp_selection(&mut self) {
        if self.entries.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.entries.len() {
            self.selected = self.entries.len() - 1;
        }
    }

    pub fn move_selection(&mut self, delta: i64) {
        if self.entries.is_empty() {
            return;
        }
        let len = self.entries.len() as i64;
        self.selected = (self.selected as i64 + delta).clamp(0, len - 1) as usize;
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.entries
            .get(self.selected)
            .map(|e| self.dir.join(&e.name))
    }

    pub fn selected_is_dir(&self) -> bool {
        self.entries.get(self.selected).is_some_and(|e| e.is_dir())
    }

    /// Enter the selected directory (if it is one).
    pub fn descend(&mut self) -> io::Result<()> {
        let Some(path) = self.selected_path() else {
            return Ok(());
        };
        if !self.selected_is_dir() {
            return Ok(());
        }
        if self.entries[self.selected].kind == Kind::Parent {
            // normalize ".." so the sidebar title shows the real path
            let Some(parent) = self.dir.parent() else {
                return Ok(());
            };
            if parent.as_os_str().is_empty() || parent == self.dir {
                return Ok(());
            }
            self.dir = parent.to_path_buf();
        } else {
            self.dir = path;
        }
        self.selected = 0;
        self.scroll = 0;
        self.reload()
    }

    /// Go up one level, re-selecting the directory we came from.
    pub fn ascend(&mut self) -> io::Result<()> {
        let Some(parent) = self.dir.parent() else {
            return Ok(());
        };
        if parent.as_os_str().is_empty() || parent == self.dir {
            return Ok(());
        }
        let prev_name = self
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        self.dir = parent.to_path_buf();
        self.reload()?;
        if let Some(name) = prev_name {
            self.select_name(&name);
        }
        Ok(())
    }

    /// Highlight the entry with the given name, if present.
    pub fn select_name(&mut self, name: &str) {
        if let Some(i) = self.entries.iter().position(|e| e.name == name) {
            self.selected = i;
        }
    }

    /// Scroll so the selection is visible in a `height`-row viewport.
    pub fn ensure_selected_visible(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + height {
            self.scroll = self.selected.saturating_add(1).saturating_sub(height);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        // unique root per test so parallel tests don't clash
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!("ratatata-sidebar-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sorts_dirs_first_then_files_case_insensitive() {
        let dir = scratch("sort");
        fs::write(dir.join("beta.txt"), "x").unwrap();
        fs::write(dir.join("Alpha.txt"), "x").unwrap();
        fs::create_dir(dir.join("Zebra")).unwrap();
        fs::create_dir(dir.join("apple")).unwrap();
        fs::write(dir.join(".hidden"), "x").unwrap();

        let sb = Sidebar::new(dir).unwrap();
        let names: Vec<&str> = sb.entries.iter().map(|e| e.name.as_str()).collect();
        // ".." first (temp dir has a parent), then dirs, then files.
        // Dotfiles are hidden by default, including `.hidden`.
        assert_eq!(names, vec!["..", "apple", "Zebra", "Alpha.txt", "beta.txt"]);
    }

    #[test]
    fn hide_dotfiles_toggle_lists_or_omits_dotfiles_but_keeps_parent() {
        let dir = scratch("dotfiles");
        fs::write(dir.join("visible.txt"), "x").unwrap();
        fs::write(dir.join(".env"), "SECRET=1").unwrap();
        fs::create_dir(dir.join(".git")).unwrap();

        let mut sb = Sidebar::new(dir).unwrap();
        assert!(sb.hide_dotfiles);
        let hidden: Vec<&str> = sb.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(hidden, vec!["..", "visible.txt"]);
        assert!(hidden.iter().any(|n| *n == ".."));
        assert!(!hidden.iter().any(|n| *n == ".env" || *n == ".git"));

        sb.hide_dotfiles = false;
        sb.reload().unwrap();
        let shown: Vec<&str> = sb.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(shown, vec!["..", ".git", ".env", "visible.txt"]);

        sb.hide_dotfiles = true;
        sb.reload().unwrap();
        let hidden_again: Vec<&str> = sb.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(hidden_again, vec!["..", "visible.txt"]);
    }

    #[test]
    fn root_has_no_parent_entry() {
        let sb = Sidebar::new(PathBuf::from("/")).unwrap();
        assert!(sb.entries.iter().all(|e| e.kind != Kind::Parent));
    }

    #[test]
    fn descend_and_ascend_round_trip() {
        let dir = scratch("nav");
        fs::create_dir(dir.join("inner")).unwrap();
        fs::write(dir.join("inner").join("f.txt"), "x").unwrap();

        let mut sb = Sidebar::new(dir.clone()).unwrap();
        sb.select_name("inner");
        assert!(sb.selected_is_dir());
        sb.descend().unwrap();
        assert_eq!(sb.dir, dir.join("inner"));
        assert_eq!(sb.entries.len(), 2); // .. and f.txt
        sb.ascend().unwrap();
        assert_eq!(sb.dir, dir);
        assert_eq!(sb.entries[sb.selected].name, "inner");
    }

    #[test]
    fn selection_clamps_to_entries() {
        let dir = scratch("clamp");
        fs::write(dir.join("a.txt"), "x").unwrap();
        let mut sb = Sidebar::new(dir).unwrap();
        sb.selected = 999;
        sb.reload().unwrap();
        assert_eq!(sb.selected, sb.entries.len() - 1);
        sb.move_selection(100);
        assert_eq!(sb.selected, sb.entries.len() - 1);
        sb.move_selection(-100);
        assert_eq!(sb.selected, 0);
    }

    #[test]
    fn ensure_selected_visible_scrolls() {
        let dir = scratch("scroll");
        for i in 0..30 {
            fs::write(dir.join(format!("f{i:02}.txt")), "x").unwrap();
        }
        let mut sb = Sidebar::new(dir).unwrap();
        sb.selected = 25;
        sb.ensure_selected_visible(10);
        assert_eq!(sb.scroll, 16);
        sb.selected = 3;
        sb.ensure_selected_visible(10);
        assert_eq!(sb.scroll, 3);
    }

    #[test]
    fn descend_on_file_is_noop() {
        let dir = scratch("noop");
        fs::write(dir.join("f.txt"), "x").unwrap();
        let mut sb = Sidebar::new(dir.clone()).unwrap();
        sb.select_name("f.txt");
        sb.descend().unwrap();
        assert_eq!(sb.dir, dir);
    }

    #[test]
    fn descend_on_parent_normalizes_path() {
        let dir = scratch("parent");
        fs::create_dir(dir.join("sub")).unwrap();
        let mut sb = Sidebar::new(dir.join("sub")).unwrap();
        sb.select_name("..");
        sb.descend().unwrap();
        assert_eq!(sb.dir, dir); // not dir.join("sub").join("..")
    }
}
