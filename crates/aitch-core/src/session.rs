//! What Aitch remembers between runs, and what it keeps in case it dies.
//!
//! Two separate jobs that share a directory:
//!
//! - **Session restore** is a convenience. Reopening yesterday's files is
//!   nice; getting it wrong costs nothing, because everything it names is
//!   already safely on disk. A session that will not load is discarded.
//! - **Recovery** is not a convenience. It holds text that exists *nowhere
//!   else* — a modified buffer that was never saved when the power went out.
//!   So a recovery file is written before it is needed, kept until the buffer
//!   is saved, and never deleted on a path where it might still be the only
//!   copy.
//!
//! The recovery file records the path it belongs to, so restoring it can say
//! what it is rather than handing back an anonymous blob.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One remembered buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenFile {
    pub path: PathBuf,
    /// Zero-based, as the buffer counts them.
    #[serde(default)]
    pub line: usize,
    #[serde(default)]
    pub column: usize,
}

/// What was open last time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// The folder, if one was open.
    #[serde(default)]
    pub root: Option<PathBuf>,
    #[serde(default)]
    pub files: Vec<OpenFile>,
    /// Which of `files` was in front.
    #[serde(default)]
    pub active: usize,
}

impl Session {
    pub fn is_empty(&self) -> bool {
        self.root.is_none() && self.files.is_empty()
    }

    /// Where the session file lives.
    pub fn default_path() -> Option<PathBuf> {
        state_dir().map(|dir| dir.join("session.toml"))
    }

    /// Read the last session, or nothing if there is not a usable one.
    ///
    /// A session that will not parse is simply discarded: it describes files
    /// that are all still on disk, so nothing is lost by forgetting it, and
    /// refusing to start over a stale convenience file would be absurd.
    pub fn load() -> Option<Session> {
        let path = Session::default_path()?;
        Session::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Option<Session> {
        let text = std::fs::read_to_string(path).ok()?;
        let session: Session = toml::from_str(&text).ok()?;
        if session.is_empty() {
            return None;
        }
        Some(session)
    }

    /// Write the session out. Failure is not worth reporting: it costs a
    /// convenience, not any work.
    pub fn save(&self) {
        let Some(path) = Session::default_path() else {
            return;
        };
        self.save_to(&path);
    }

    pub fn save_to(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = toml::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }

    /// Forget the session, when the editor is closed deliberately with
    /// nothing open worth remembering.
    pub fn clear() {
        if let Some(path) = Session::default_path() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Unsaved work, held in case the editor does not come back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recovery {
    /// The file this was a modified copy of, if it had a name.
    pub path: Option<PathBuf>,
    /// When it was written, for telling the user how old it is.
    pub saved_at: String,
    pub text: String,
}

impl Recovery {
    /// Where recovery files live.
    pub fn directory() -> Option<PathBuf> {
        state_dir().map(|dir| dir.join("recovery"))
    }

    /// A stable filename for a buffer, so its recovery file is overwritten
    /// rather than accumulating one copy per keystroke run.
    fn file_name(path: Option<&Path>, index: usize) -> String {
        match path {
            Some(path) => format!("{:016x}.toml", stable_hash(&path.to_string_lossy())),
            // An unnamed buffer has nothing to key on but its position.
            None => format!("scratch-{index}.toml"),
        }
    }

    /// Write a recovery file for one buffer.
    pub fn write(path: Option<&Path>, index: usize, text: &str) -> Option<PathBuf> {
        let directory = Recovery::directory()?;
        std::fs::create_dir_all(&directory).ok()?;
        let file = directory.join(Recovery::file_name(path, index));

        let recovery = Recovery {
            path: path.map(Path::to_path_buf),
            saved_at: timestamp(),
            text: text.to_string(),
        };
        let encoded = toml::to_string(&recovery).ok()?;
        std::fs::write(&file, encoded).ok()?;
        Some(file)
    }

    /// Remove a buffer's recovery file, once its work is safely on disk.
    pub fn discard(path: Option<&Path>, index: usize) {
        let Some(directory) = Recovery::directory() else {
            return;
        };
        let _ = std::fs::remove_file(directory.join(Recovery::file_name(path, index)));
    }

    /// Every recovery file waiting to be dealt with.
    pub fn pending() -> Vec<Recovery> {
        let Some(directory) = Recovery::directory() else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(&directory) else {
            return Vec::new();
        };

        let mut found = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            // A recovery file that will not parse is left where it is: it may
            // still be the only copy of somebody's work, and deleting it to
            // tidy up would be the worst thing this module could do.
            if let Ok(recovery) = toml::from_str::<Recovery>(&text) {
                found.push(recovery);
            }
        }
        found.sort_by(|a, b| a.saved_at.cmp(&b.saved_at));
        found
    }

    /// How this reads when offering it back.
    pub fn describe(&self) -> String {
        let name = match &self.path {
            Some(path) => path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string()),
            None => "an unsaved buffer".to_string(),
        };
        format!("{name} ({} bytes, {})", self.text.len(), self.saved_at)
    }
}

/// Where Aitch keeps what it remembers.
fn state_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|base| PathBuf::from(base).join("aitch"))
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("state"))
            })
            .map(|base| base.join("aitch"))
    }
}

/// Seconds since the epoch, as text. Enough to sort by and to show.
fn timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// FNV-1a. Not for security — only for turning a path into a stable filename.
fn stable_hash(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aitch-session-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_session_round_trips() {
        let dir = scratch("round-trip");
        let path = dir.join("session.toml");

        let session = Session {
            root: Some(PathBuf::from("/home/someone/project")),
            files: vec![
                OpenFile {
                    path: PathBuf::from("src/main.rs"),
                    line: 41,
                    column: 8,
                },
                OpenFile {
                    path: PathBuf::from("README.md"),
                    line: 0,
                    column: 0,
                },
            ],
            active: 1,
        };
        session.save_to(&path);

        assert_eq!(Session::load_from(&path), Some(session));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_broken_session_is_discarded_rather_than_fatal() {
        let dir = scratch("broken");
        let path = dir.join("session.toml");
        std::fs::write(&path, "this is not = = toml").unwrap();

        assert_eq!(Session::load_from(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_empty_session_is_nothing_to_restore() {
        let dir = scratch("empty");
        let path = dir.join("session.toml");
        Session::default().save_to(&path);

        assert_eq!(Session::load_from(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_session_is_nothing_to_restore() {
        assert_eq!(
            Session::load_from(Path::new("/no/such/session/anywhere.toml")),
            None
        );
    }

    #[test]
    fn a_session_keeps_where_the_cursor_was() {
        let dir = scratch("cursor");
        let path = dir.join("session.toml");
        Session {
            root: None,
            files: vec![OpenFile {
                path: PathBuf::from("notes.txt"),
                line: 120,
                column: 4,
            }],
            active: 0,
        }
        .save_to(&path);

        let restored = Session::load_from(&path).unwrap();
        assert_eq!(restored.files[0].line, 120);
        assert_eq!(restored.files[0].column, 4);
        let _ = std::fs::remove_dir_all(dir);
    }

    // -- recovery ----------------------------------------------------------

    #[test]
    fn a_recovery_file_names_the_buffer_it_came_from() {
        let recovery = Recovery {
            path: Some(PathBuf::from("/home/someone/notes.txt")),
            saved_at: "1700000000".to_string(),
            text: "unsaved work".to_string(),
        };
        assert!(recovery.describe().starts_with("notes.txt"));
        assert!(recovery.describe().contains("12 bytes"));
    }

    #[test]
    fn an_unnamed_buffer_still_describes_itself() {
        let recovery = Recovery {
            path: None,
            saved_at: "1700000000".to_string(),
            text: "x".to_string(),
        };
        assert!(recovery.describe().starts_with("an unsaved buffer"));
    }

    #[test]
    fn one_buffer_keeps_one_recovery_file_however_often_it_is_written() {
        // Otherwise a long editing session leaves a directory full of
        // near-identical copies and no way to tell which is current.
        let path = Path::new("/home/someone/notes.txt");
        let first = Recovery::file_name(Some(path), 0);
        let again = Recovery::file_name(Some(path), 7);
        assert_eq!(first, again, "the name comes from the path, not the slot");

        let other = Recovery::file_name(Some(Path::new("/home/someone/other.txt")), 0);
        assert_ne!(first, other);
    }

    #[test]
    fn unnamed_buffers_get_a_name_each() {
        assert_ne!(
            Recovery::file_name(None, 0),
            Recovery::file_name(None, 1),
            "two unsaved scratch buffers must not overwrite each other"
        );
    }

    #[test]
    fn a_recovery_file_round_trips_through_toml() {
        let recovery = Recovery {
            path: Some(PathBuf::from("notes.txt")),
            saved_at: "1700000000".to_string(),
            // Awkward content: quotes, newlines, and a tab.
            text: "line one\n\t\"quoted\"\nline three\n".to_string(),
        };
        let encoded = toml::to_string(&recovery).unwrap();
        let decoded: Recovery = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, recovery, "recovery must not mangle the text");
    }

    #[test]
    fn the_hash_is_stable_across_calls() {
        assert_eq!(stable_hash("src/main.rs"), stable_hash("src/main.rs"));
        assert_ne!(stable_hash("a"), stable_hash("b"));
    }
}
