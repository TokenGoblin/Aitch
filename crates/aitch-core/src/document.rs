//! One open file: its buffer, where it came from, and how to put it back.
//!
//! Not in PLAN.md §3's module list, which jumps from `fileio.rs` to
//! `workspace.rs`. Phase 2 needs somewhere to keep the path and encoding a
//! save depends on, and putting them in the UI would scatter the byte-exact
//! round-trip across two crates. Phase 4's `workspace.rs` owns a set of these.

use std::path::{Path, PathBuf};

use crate::buffer::Buffer;
use crate::fileio::{self, Encoding, FileError};

/// A buffer plus the file it belongs to.
#[derive(Debug)]
pub struct Document {
    pub buffer: Buffer,
    path: Option<PathBuf>,
    /// How the file was encoded when read, so it can be written back the same
    /// way. A new file gets UTF-8 without a BOM.
    encoding: Encoding,
}

impl Document {
    /// An unnamed, empty buffer.
    pub fn blank() -> Document {
        Document {
            buffer: Buffer::new(),
            path: None,
            encoding: Encoding::UTF8,
        }
    }

    /// Read a file, keeping everything needed to write it back unchanged.
    pub fn open(path: &Path) -> Result<Document, FileError> {
        let loaded = fileio::load(path)?;
        let mut buffer = Buffer::from_str(&loaded.text);
        buffer.set_line_ending(loaded.line_ending);
        Ok(Document {
            buffer,
            path: Some(path.to_path_buf()),
            encoding: loaded.encoding,
        })
    }

    /// A file that does not exist yet: an empty buffer that knows its name.
    pub fn new_at(path: &Path) -> Document {
        Document {
            buffer: Buffer::new(),
            path: Some(path.to_path_buf()),
            encoding: Encoding::UTF8,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    pub fn is_dirty(&self) -> bool {
        self.buffer.is_dirty()
    }

    /// What to call this in a title bar or a status line.
    pub fn display_name(&self) -> String {
        match &self.path {
            Some(path) => path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string()),
            None => "New Buffer".to_string(),
        }
    }

    /// Write the buffer back to its file.
    ///
    /// A document with no path cannot be saved without asking the user for
    /// one, and asking needs the prompt line — which is Phase 3. Until then
    /// this reports [`FileError::NoPath`] and the caller says so.
    pub fn save(&mut self) -> Result<(), FileError> {
        let path = self.path.clone().ok_or(FileError::NoPath)?;
        fileio::save(&path, &self.buffer.text().to_string(), self.encoding)?;
        self.buffer.mark_saved();
        Ok(())
    }
}

impl Default for Document {
    fn default() -> Document {
        Document::blank()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Position;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aitch-doc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn a_blank_document_has_no_name_and_nothing_to_save() {
        let mut doc = Document::blank();
        assert_eq!(doc.display_name(), "New Buffer");
        assert!(!doc.is_dirty());
        assert!(matches!(doc.save(), Err(FileError::NoPath)));
    }

    #[test]
    fn open_edit_save_keeps_the_encoding_it_arrived_with() {
        let path = scratch("crlf.txt");
        std::fs::write(&path, b"alpha\r\nbeta\r\n").unwrap();

        let mut doc = Document::open(&path).unwrap();
        assert_eq!(doc.display_name(), "crlf.txt");
        assert!(!doc.is_dirty());

        doc.buffer.set_cursor(Position::new(0, 0));
        doc.buffer.insert("X");
        assert!(doc.is_dirty());

        doc.save().unwrap();
        assert!(!doc.is_dirty(), "saving cleans the buffer");
        assert_eq!(std::fs::read(&path).unwrap(), b"Xalpha\r\nbeta\r\n");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_new_file_is_written_as_utf8() {
        let path = scratch("brand-new.txt");
        std::fs::remove_file(&path).ok();

        let mut doc = Document::new_at(&path);
        doc.buffer.insert("hello");
        doc.save().unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        std::fs::remove_file(&path).ok();
    }
}
