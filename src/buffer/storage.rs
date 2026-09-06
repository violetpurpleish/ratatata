//! File loading, line-ending preservation, and atomic saving.

use super::*;

/// Stage writes beside the destination so publication is an atomic operation.
/// Resolve existing symlinks to retain the link itself when saving its target.
pub(super) fn atomic_save(
    path: &std::path::Path,
    overwrite: bool,
    write: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let target = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() && overwrite => fs::canonicalize(path)?,
        Ok(_) | Err(_) => path.to_path_buf(),
    };
    let metadata = match fs::metadata(&target) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if let Some(meta) = &metadata {
        if !overwrite {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "file already exists",
            ));
        }
        if !meta.is_file() || meta.permissions().readonly() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "destination is not a writable regular file",
            ));
        }
    }
    if metadata.is_some() {
        // Check file write access too; directory access alone must not grant
        // permission to replace a file the user could not otherwise edit.
        fs::OpenOptions::new().write(true).open(&target)?;
    }
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    write(temp.as_file_mut())?;
    if let Some(meta) = metadata {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let staged = temp.as_file().metadata()?;
            if staged.uid() != meta.uid() || staged.gid() != meta.gid() {
                std::os::unix::fs::chown(temp.path(), Some(meta.uid()), Some(meta.gid()))?;
            }
        }
        temp.as_file().set_permissions(meta.permissions())?;
    }
    temp.as_file().sync_all()?;
    if overwrite {
        temp.persist(&target).map_err(|e| e.error)?;
    } else {
        temp.persist_noclobber(&target).map_err(|e| e.error)?;
    }
    Ok(())
}

impl Buffer {
    /// Load a file, keeping CRLF separators outside the editable text.
    /// Unedited files round-trip exactly; edited mixed-ending files use CRLF.
    /// A trailing newline yields a final empty line.
    pub fn from_path(path: PathBuf) -> io::Result<Self> {
        let content = fs::read_to_string(&path)?;
        let mut buf = Self::empty();
        buf.line_ending = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        buf.lines = content
            .replace("\r\n", "\n")
            .split('\n')
            .map(str::to_string)
            .collect();
        buf.clean_lines = buf.lines.clone();
        buf.clean_disk_content = content;
        buf.path = Some(path);
        buf.sync_parinfer_prev();
        Ok(buf)
    }

    /// Save to the bound path, replacing the destination only after a complete write.
    pub fn save(&mut self) -> io::Result<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "buffer has no file name"))?;
        self.save_to(path, true)
    }

    /// Bind a new path only after saving succeeds. Without overwrite consent,
    /// publishing the temporary file fails if a destination already exists.
    pub fn save_to(&mut self, path: PathBuf, overwrite: bool) -> io::Result<()> {
        let content = if self.lines == self.clean_lines {
            self.clean_disk_content.clone()
        } else {
            self.lines.join(self.line_ending)
        };
        atomic_save(&path, overwrite, |file| file.write_all(content.as_bytes()))?;
        self.clean_disk_content = content;
        self.path = Some(path);
        self.clean_lines = self.lines.clone();
        self.dirty = false;
        self.last_edit = None;
        Ok(())
    }
}
