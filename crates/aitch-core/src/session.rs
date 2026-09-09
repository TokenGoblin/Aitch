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
    /// Whether that folder was opened deliberately or worked out from a file.
    ///
    /// Restoring has to know: only a folder that was asked for is watched, and
    /// a session that forgets the difference turns `aitch .` into `aitch
    /// somefile` the next time it starts. Defaulted, so a session written
    /// before this existed still loads — it simply does not watch, which is
    /// what it did anyway.
    #[serde(default)]
    pub root_opened: bool,
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
    /// Which file on disk this came out of.
    ///
    /// Not part of what is written — it is how the file names itself — but
    /// it is the only way to delete exactly the recovery that was answered
    /// for, rather than recomputing a name and hoping it still matches.
    #[serde(skip)]
    file: Option<PathBuf>,
}

impl Recovery {
    /// Where recovery files live.
    pub fn directory() -> Option<PathBuf> {
        state_dir().map(|dir| dir.join("recovery"))
    }

    /// A stable filename for a buffer, so its recovery file is overwritten
    /// rather than accumulating one copy per keystroke run.
    fn file_name(path: Option<&Path>, key: u64) -> String {
        match path {
            Some(path) => format!(
                "{:016x}.toml",
                stable_hash(&recovery_identity(path).to_string_lossy())
            ),
            // An unnamed buffer has no path to key on, so it carries a token
            // unique to the run that made it. A position would do until the
            // next run opened its own scratch buffer at the same position and
            // deleted work it knew nothing about.
            None => format!("scratch-{key:016x}.toml"),
        }
    }

    /// Write a recovery file for one buffer.
    pub fn write(path: Option<&Path>, key: u64, text: &str) -> Option<PathBuf> {
        let directory = Recovery::directory()?;
        std::fs::create_dir_all(&directory).ok()?;
        let file = directory.join(Recovery::file_name(path, key));

        let recovery = Recovery {
            path: path.map(recovery_identity),
            saved_at: timestamp(),
            text: text.to_string(),
            file: None,
        };
        let encoded = toml::to_string(&recovery).ok()?;
        std::fs::write(&file, encoded).ok()?;
        Some(file)
    }

    /// Remove a buffer's recovery file, once its work is safely on disk.
    pub fn discard(path: Option<&Path>, key: u64) {
        let Some(directory) = Recovery::directory() else {
            return;
        };
        let _ = std::fs::remove_file(directory.join(Recovery::file_name(path, key)));
    }

    /// Delete exactly this recovery file.
    ///
    /// For the one case where deleting is right: the person was shown the
    /// work and said they did not want it.
    pub fn remove(&self) {
        if let Some(file) = &self.file {
            let _ = std::fs::remove_file(file);
        }
    }

    /// A recovery not backed by a file, for tests and for offering text that
    /// was read some other way.
    pub fn detached(path: Option<PathBuf>, saved_at: &str, text: &str) -> Recovery {
        Recovery {
            path,
            saved_at: saved_at.to_string(),
            text: text.to_string(),
            file: None,
        }
    }

    /// Write one into a directory named outright, for tests.
    pub fn write_in(directory: &Path, path: Option<&Path>, key: u64, text: &str) -> PathBuf {
        std::fs::create_dir_all(directory).expect("recovery directory");
        let file = directory.join(Recovery::file_name(path, key));
        let recovery = Recovery {
            path: path.map(recovery_identity),
            saved_at: timestamp(),
            text: text.to_string(),
            file: None,
        };
        std::fs::write(&file, toml::to_string(&recovery).expect("encode")).expect("write");
        file
    }

    /// Every recovery file waiting to be dealt with.
    pub fn pending() -> Vec<Recovery> {
        match Recovery::directory() {
            Some(directory) => Recovery::pending_in(&directory),
            None => Vec::new(),
        }
    }

    /// The same, over a directory named outright.
    ///
    /// Split out so that reading recovery files can be tested without an
    /// environment variable: the state directory is process-wide, and tests
    /// that set it would race each other.
    pub fn pending_in(directory: &Path) -> Vec<Recovery> {
        let Ok(entries) = std::fs::read_dir(directory) else {
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
            if let Ok(mut recovery) = toml::from_str::<Recovery>(&text) {
                recovery.file = Some(entry.path());
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

/// Where a startup failure is written down.
pub fn failure_log() -> Option<PathBuf> {
    state_dir().map(|dir| dir.join("startup.log"))
}

/// Record something that stopped the editor starting.
///
/// A GUI program that dies before its first frame has nowhere to say why. The
/// message goes to stderr, but `aitch` is a console-subsystem binary: started
/// from the Start menu, Windows gives it a console that closes with the
/// process, so the explanation exists for a frame and is gone. A panic is
/// worse still — it never reaches the code that prints anything.
///
/// So it goes in a file as well. Appended rather than replaced, and capped,
/// because the second failure is often the informative one and a log nobody
/// prunes is its own bug.
pub fn log_failure(message: &str) {
    let Some(path) = failure_log() else {
        return;
    };
    let Some(directory) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(directory).is_err() {
        return;
    }

    // One line per failure, however many lines the message itself has: a log
    // that can be skimmed is worth more than one that preserves formatting.
    let flattened = message.replace(['\n', '\r'], " ");
    let entry = format!("{} {}\n", timestamp(), flattened.trim());

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let kept: Vec<&str> = existing
        .lines()
        .rev()
        .take(FAILURE_LOG_LINES - 1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&entry);
    let _ = std::fs::write(&path, out);
}

/// How many past failures the log keeps.
const FAILURE_LOG_LINES: usize = 20;

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

/// When it is now, as `YYYY-MM-DD HH:MM UTC`.
///
/// Sorts correctly as text, which is what `pending` needs, and reads as a
/// date, which is what the person being asked "restore this?" needs. UTC
/// rather than local time because a timezone database is a dependency and
/// this is a line in a prompt.
fn timestamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_utc(seconds)
}

/// Epoch seconds as `YYYY-MM-DD HH:MM UTC`.
///
/// Howard Hinnant's civil-from-days, which shifts the year to start in March
/// so that the leap day lands at the end of the cycle and needs no special
/// case. Cheaper than a date crate for the one place a date is shown.
fn format_utc(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = if month <= 2 { year + 1 } else { year };

    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// The path a recovery file is keyed and matched on.
///
/// Not the path as it was written. `aitch notes.txt` run in two different
/// folders stores the same relative path both times, so the two buffers keyed
/// to one recovery file: the second run overwrote the first run's unsaved
/// work, and then offered what was left back as the wrong file. And on
/// Windows `C:\X\A.TXT` and `c:\x\a.txt` are one file that keyed to two,
/// leaving behind a recovery nothing would ever clear away.
///
/// Resolved against the filesystem where that is possible and made absolute
/// where it is not: a file that does not exist yet cannot be canonicalized,
/// but its folder usually can, which is enough to tell two folders apart.
pub fn recovery_identity(path: &Path) -> PathBuf {
    let resolved = path
        .canonicalize()
        .or_else(|_| match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
                parent.canonicalize().map(|parent| parent.join(name))
            }
            _ => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        })
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf());

    if !cfg!(windows) {
        return resolved;
    }

    // Two Windows-only adjustments, both so that the same file resolved by
    // different routes above compares equal.
    //
    // `canonicalize` returns the extended-length form, `\\?\C:\...`, and the
    // fallback for a file that does not exist yet returns a plain `C:\...`.
    // Left alone, creating a file would change its recovery key and orphan
    // whatever had already been written for it.
    let text = resolved.to_string_lossy();
    let plain = match text.strip_prefix(r"\\?\") {
        // `\\?\UNC\server\share` is the verbatim spelling of `\\server\share`.
        Some(rest) => match rest.strip_prefix("UNC\\") {
            Some(unc) => format!(r"\\{unc}"),
            None => rest.to_string(),
        },
        None => text.to_string(),
    };

    // And the filesystem is case-insensitive, so two spellings of one file
    // must not key to two recovery files.
    PathBuf::from(plain.to_lowercase())
}

/// FNV-1a. Not for security — only for turning a path into a stable filename.
fn stable_hash(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod failure_log_tests {
    use super::*;

    #[test]
    fn a_failure_becomes_one_line_however_many_it_had() {
        // Skimmable beats faithful: a wgpu validation error arrives as a
        // paragraph, and a log with one entry per failure can be read at a
        // glance and tailed.
        let raw = "wgpu error: Validation Error\n\nCaused by:\n  too large\n";
        let flattened = raw.replace(['\n', '\r'], " ");

        assert!(!flattened.contains('\n'));
        assert!(flattened.contains("Validation Error"));
        assert!(flattened.contains("too large"));
    }

    #[test]
    fn the_log_keeps_the_most_recent_failures_and_drops_the_rest() {
        // The second failure is usually the informative one, so this keeps
        // history -- but a log nobody prunes is its own bug.
        let existing: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let kept: Vec<&str> = existing
            .iter()
            .map(String::as_str)
            .rev()
            .take(FAILURE_LOG_LINES - 1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        assert_eq!(kept.len(), FAILURE_LOG_LINES - 1);
        assert_eq!(*kept.last().expect("a last line"), "line 49", "newest kept");
        assert_eq!(kept[0], "line 31", "oldest dropped");
    }

    #[test]
    fn the_log_sits_beside_the_other_state() {
        // Whatever the platform, it belongs with the sessions and the
        // recovery files rather than somewhere of its own.
        if let (Some(log), Some(recovery)) = (failure_log(), Recovery::directory()) {
            assert_eq!(log.parent(), recovery.parent());
            assert_eq!(
                log.file_name().and_then(|n| n.to_str()),
                Some("startup.log")
            );
        }
    }
}

#[cfg(test)]
mod recovery_identity_tests {
    use super::*;

    #[test]
    fn the_key_does_not_depend_on_how_the_path_was_written() {
        // `aitch notes.txt` and `aitch /full/path/notes.txt` are the same
        // file and must own the same recovery file.
        let directory = std::env::current_dir().expect("a working directory");
        let relative = Path::new("Cargo.toml");
        let absolute = directory.join("Cargo.toml");

        assert_eq!(
            Recovery::file_name(Some(relative), 0),
            Recovery::file_name(Some(&absolute), 0),
        );
    }

    #[test]
    fn the_same_name_in_two_folders_keys_to_two_recoveries() {
        // The data loss this exists to stop: two projects, each with a
        // notes.txt, both opened as `aitch notes.txt`. They hashed the same
        // relative string, so the second run overwrote the first run's
        // unsaved work and then offered it back as the wrong file.
        let a = Path::new("project-a").join("notes.txt");
        let b = Path::new("project-b").join("notes.txt");

        assert_ne!(
            Recovery::file_name(Some(&a), 0),
            Recovery::file_name(Some(&b), 0),
        );
    }

    #[test]
    #[cfg(windows)]
    fn two_spellings_of_one_windows_path_key_to_one_recovery() {
        // Case-insensitive filesystem: these are one file, and two recovery
        // files for it means one of them is never cleared away.
        let upper = Path::new(r"C:\Projects\NOTES.TXT");
        let lower = Path::new(r"c:\projects\notes.txt");

        assert_eq!(
            Recovery::file_name(Some(upper), 0),
            Recovery::file_name(Some(lower), 0),
        );
    }

    #[test]
    fn a_file_that_does_not_exist_yet_still_gets_a_folder_specific_key() {
        // Opening a name that is not on disk is ordinary -- it is how you
        // start a new file -- and it must not collapse to the bare name.
        let directory = std::env::current_dir().expect("a working directory");
        let here = directory.join("not-created-yet.txt");
        let elsewhere = directory.join("sub").join("not-created-yet.txt");

        assert_ne!(
            Recovery::file_name(Some(&here), 0),
            Recovery::file_name(Some(&elsewhere), 0),
        );
        assert_eq!(
            Recovery::file_name(Some(Path::new("not-created-yet.txt")), 0),
            Recovery::file_name(Some(&here), 0),
            "and it still matches the same file named the short way"
        );
    }
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
            root_opened: true,
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
            root_opened: true,
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

    #[test]
    fn the_hash_is_the_one_it_says_it_is() {
        // FNV-1a's published vectors. An extra digit in the prime still gives
        // a usable hash, which is exactly why it would never be noticed.
        assert_eq!(stable_hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(stable_hash("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(stable_hash("foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn a_timestamp_reads_as_a_date() {
        // Someone deciding whether to restore work needs to know when it is
        // from, and epoch seconds do not tell them.
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14 22:13:20 UTC");
        // A leap day, which is where a hand-rolled calendar goes wrong.
        assert_eq!(format_utc(1_709_208_000), "2024-02-29 12:00:00 UTC");
        assert_eq!(format_utc(4_102_444_800), "2100-01-01 00:00:00 UTC");
    }

    #[test]
    fn timestamps_sort_the_way_they_read() {
        // `pending` sorts on this string to find the newest recovery, so it
        // has to resolve finer than the gap between two of them. Minutes did
        // not: two saves a few keystrokes apart sorted arbitrarily.
        assert!(format_utc(1_700_000_000) < format_utc(1_709_208_000));
        assert!(
            format_utc(0) < format_utc(1),
            "one second apart still sorts"
        );
    }

    // -- recovery ----------------------------------------------------------

    #[test]
    fn removing_a_recovery_removes_the_file_it_came_from() {
        let directory = scratch("recovery-remove").join("recovery");
        Recovery::write_in(&directory, Some(Path::new("one.txt")), 0, "first");
        Recovery::write_in(&directory, Some(Path::new("two.txt")), 0, "second");

        let pending = Recovery::pending_in(&directory);
        assert_eq!(pending.len(), 2);

        let one = pending
            .iter()
            .find(|r| r.text == "first")
            .expect("the first one");
        one.remove();

        let left = Recovery::pending_in(&directory);
        assert_eq!(left.len(), 1, "only the one that was answered for");
        assert_eq!(left[0].text, "second");
    }

    #[test]
    fn two_unnamed_buffers_do_not_share_a_recovery_file() {
        // The bug this guards: keying an unnamed buffer on its position means
        // the next run's scratch buffer at position 0 deletes the crashed
        // run's work at position 0, which nothing had offered back yet.
        let directory = scratch("recovery-scratch").join("recovery");
        Recovery::write_in(&directory, None, 0x1111_0000, "one run's work");
        Recovery::write_in(&directory, None, 0x2222_0000, "another run's work");

        assert_eq!(Recovery::pending_in(&directory).len(), 2);
    }

    #[test]
    fn a_recovery_file_names_the_buffer_it_came_from() {
        let recovery = Recovery::detached(
            Some(PathBuf::from("/home/someone/notes.txt")),
            "1700000000",
            "unsaved work",
        );
        assert!(recovery.describe().starts_with("notes.txt"));
        assert!(recovery.describe().contains("12 bytes"));
    }

    #[test]
    fn an_unnamed_buffer_still_describes_itself() {
        let recovery = Recovery::detached(None, "1700000000", "x");
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
        let recovery = Recovery::detached(
            Some(PathBuf::from("notes.txt")),
            "1700000000",
            "line one\n\t\"quoted\"\nline three\n",
        );
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
