//! Loading and saving files, byte-exactly.
//!
//! The rule this file exists to keep: **load, make one edit, save, and the
//! result differs from the original only by that edit.** Not "differs only in
//! ways that do not matter" — byte-identical. An editor that silently rewrites
//! line endings or re-encodes a file is an editor that corrupts diffs, breaks
//! checksums and loses data it was never asked to touch.
//!
//! Three things follow from it:
//!
//! - **The source encoding is remembered and written back.** A UTF-16 file
//!   stays UTF-16, a BOM that was there stays there, and one that was not is
//!   not added.
//! - **Line breaks are never normalized.** They are decoded into the buffer as
//!   they were and written out unchanged. Only a *newly typed* newline uses the
//!   file's dominant ending; see [`crate::line_ending`].
//! - **Detection refuses to guess.** The supported set is the one PLAN.md
//!   names — UTF-8, UTF-16 LE/BE, and Latin-1 as the fallback that cannot fail.
//!   No charset sniffing beyond that: a wrong guess would be written back as
//!   fact on the next save, which is the failure this file is built to avoid.
//!
//! Saving is atomic: write a temporary file beside the target, flush it, then
//! rename over. A crash mid-write leaves the original intact.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::line_ending::LineEnding;

/// How the bytes of a file map to text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// ISO-8859-1. Every byte is a character, so it always decodes and always
    /// round-trips — which is what makes it the fallback.
    Latin1,
}

impl Charset {
    pub fn name(self) -> &'static str {
        match self {
            Charset::Utf8 => "UTF-8",
            Charset::Utf16Le => "UTF-16LE",
            Charset::Utf16Be => "UTF-16BE",
            Charset::Latin1 => "Latin-1",
        }
    }
}

/// A charset plus whether the file carried a byte-order mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Encoding {
    pub charset: Charset,
    /// A BOM that was present is written back; one that was absent is not
    /// added. Users notice when a tool starts prefixing their files.
    pub bom: bool,
}

impl Encoding {
    pub const UTF8: Encoding = Encoding {
        charset: Charset::Utf8,
        bom: false,
    };

    pub fn new(charset: Charset, bom: bool) -> Encoding {
        Encoding { charset, bom }
    }

    pub fn describe(&self) -> String {
        if self.bom {
            format!("{} with BOM", self.charset.name())
        } else {
            self.charset.name().to_string()
        }
    }
}

impl Default for Encoding {
    fn default() -> Encoding {
        Encoding::UTF8
    }
}

const BOM_UTF8: &[u8] = &[0xEF, 0xBB, 0xBF];
const BOM_UTF16_LE: &[u8] = &[0xFF, 0xFE];
const BOM_UTF16_BE: &[u8] = &[0xFE, 0xFF];

/// How much of a file to look at when guessing. A header this long settles it
/// for any real text file, and bounds the cost for a huge one.
const SNIFF_LIMIT: usize = 8192;

/// A file, decoded, with everything needed to write it back unchanged.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub text: String,
    pub encoding: Encoding,
    /// The ending a newly typed newline should use.
    pub line_ending: LineEnding,
}

/// Work out how a file is encoded.
///
/// Order matters. The UTF-16 check comes before the UTF-8 one because ASCII
/// text in UTF-16LE (`h\0e\0l\0l\0o\0`) is also perfectly valid UTF-8 — NUL is
/// a legal UTF-8 character — so checking UTF-8 first would decode a UTF-16 file
/// into text full of NULs and then write that back as UTF-8.
pub fn detect(bytes: &[u8]) -> Encoding {
    if bytes.starts_with(BOM_UTF8) {
        return Encoding::new(Charset::Utf8, true);
    }
    if bytes.starts_with(BOM_UTF16_LE) {
        return Encoding::new(Charset::Utf16Le, true);
    }
    if bytes.starts_with(BOM_UTF16_BE) {
        return Encoding::new(Charset::Utf16Be, true);
    }

    if let Some(charset) = sniff_utf16(bytes) {
        return Encoding::new(charset, false);
    }
    if std::str::from_utf8(bytes).is_ok() {
        return Encoding::new(Charset::Utf8, false);
    }

    Encoding::new(Charset::Latin1, false)
}

/// Look for the NUL pattern that unmarked UTF-16 text leaves behind.
///
/// In UTF-16LE, ASCII is `XX 00` — NULs land on odd offsets. In UTF-16BE it is
/// `00 XX` — NULs on even offsets. Real UTF-8 text has no NULs at all, so any
/// strong, one-sided pattern is decisive; a weak or balanced one is not, and
/// returns `None` rather than a guess.
fn sniff_utf16(bytes: &[u8]) -> Option<Charset> {
    let limit = bytes.len().min(SNIFF_LIMIT) & !1; // whole code units only
    if limit < 2 {
        return None;
    }

    let mut nul_even = 0usize;
    let mut nul_odd = 0usize;
    for (i, byte) in bytes[..limit].iter().enumerate() {
        if *byte == 0 {
            if i % 2 == 0 {
                nul_even += 1;
            } else {
                nul_odd += 1;
            }
        }
    }

    // At least a fifth of the sniffed bytes must be NUL, and they must be
    // overwhelmingly on one side. Latin-1 and UTF-8 text do not do this.
    let units = limit / 2;
    let threshold = units / 5;
    if nul_odd > threshold && nul_odd > nul_even * 4 {
        Some(Charset::Utf16Le)
    } else if nul_even > threshold && nul_even > nul_odd * 4 {
        Some(Charset::Utf16Be)
    } else {
        None
    }
}

/// Decode bytes with a known encoding, stripping the BOM if there is one.
pub fn decode(bytes: &[u8], encoding: Encoding) -> Result<String, FileError> {
    let body = strip_bom(bytes, encoding);

    match encoding.charset {
        Charset::Utf8 => std::str::from_utf8(body)
            .map(str::to_string)
            .map_err(|e| FileError::Decode(format!("not valid UTF-8 at byte {}", e.valid_up_to()))),

        Charset::Utf16Le | Charset::Utf16Be => {
            if body.len() % 2 != 0 {
                return Err(FileError::Decode(
                    "UTF-16 file has an odd number of bytes".to_string(),
                ));
            }
            let little = encoding.charset == Charset::Utf16Le;
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|pair| {
                    if little {
                        u16::from_le_bytes([pair[0], pair[1]])
                    } else {
                        u16::from_be_bytes([pair[0], pair[1]])
                    }
                })
                .collect();
            String::from_utf16(&units)
                .map_err(|_| FileError::Decode("unpaired UTF-16 surrogate".to_string()))
        }

        // Every byte is a code point, so this cannot fail — which is the whole
        // point of it being the fallback.
        Charset::Latin1 => Ok(body.iter().map(|b| *b as char).collect()),
    }
}

/// Encode text back into bytes, re-attaching the BOM if there was one.
pub fn encode(text: &str, encoding: Encoding) -> Result<Vec<u8>, FileError> {
    let mut out = Vec::with_capacity(text.len() + 3);

    if encoding.bom {
        out.extend_from_slice(match encoding.charset {
            Charset::Utf8 => BOM_UTF8,
            Charset::Utf16Le => BOM_UTF16_LE,
            Charset::Utf16Be => BOM_UTF16_BE,
            // Latin-1 has no byte-order mark; detection never sets this.
            Charset::Latin1 => &[],
        });
    }

    match encoding.charset {
        Charset::Utf8 => out.extend_from_slice(text.as_bytes()),

        Charset::Utf16Le => {
            for unit in text.encode_utf16() {
                out.extend_from_slice(&unit.to_le_bytes());
            }
        }
        Charset::Utf16Be => {
            for unit in text.encode_utf16() {
                out.extend_from_slice(&unit.to_be_bytes());
            }
        }

        Charset::Latin1 => {
            for c in text.chars() {
                let code = c as u32;
                if code > 0xFF {
                    // Refusing beats writing a `?`: the user can still save
                    // as UTF-8, and nothing is silently lost either way.
                    return Err(FileError::Encode(format!(
                        "{c:?} cannot be written as Latin-1"
                    )));
                }
                out.push(code as u8);
            }
        }
    }

    Ok(out)
}

fn strip_bom(bytes: &[u8], encoding: Encoding) -> &[u8] {
    if !encoding.bom {
        return bytes;
    }
    let bom = match encoding.charset {
        Charset::Utf8 => BOM_UTF8,
        Charset::Utf16Le => BOM_UTF16_LE,
        Charset::Utf16Be => BOM_UTF16_BE,
        Charset::Latin1 => return bytes,
    };
    bytes.strip_prefix(bom).unwrap_or(bytes)
}

/// Read and decode a file.
pub fn load(path: &Path) -> Result<Loaded, FileError> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|e| FileError::Io(path.to_path_buf(), e))?;
    load_bytes(&bytes)
}

/// Decode bytes already in hand. Split out so tests need no filesystem.
pub fn load_bytes(bytes: &[u8]) -> Result<Loaded, FileError> {
    let encoding = detect(bytes);
    let text = decode(bytes, encoding)?;
    let line_ending = LineEnding::dominant(&ropey::Rope::from_str(&text));
    Ok(Loaded {
        text,
        encoding,
        line_ending,
    })
}

/// Write a file atomically: temporary file beside it, flushed, then renamed.
///
/// The temporary lives in the same directory so the rename stays within one
/// filesystem, which is what makes it atomic. A crash before the rename leaves
/// the original file untouched.
pub fn save(path: &Path, text: &str, encoding: Encoding) -> Result<(), FileError> {
    let bytes = encode(text, encoding)?;
    let temporary = temporary_path(path);

    let write = || -> io::Result<()> {
        let mut file = File::create(&temporary)?;
        file.write_all(&bytes)?;
        // Flush to the device before the rename, or a crash can leave the
        // renamed file present but empty — worse than no save at all.
        file.sync_all()?;
        Ok(())
    };

    if let Err(e) = write() {
        let _ = fs::remove_file(&temporary);
        return Err(FileError::Io(temporary, e));
    }

    fs::rename(&temporary, path).map_err(|e| {
        let _ = fs::remove_file(&temporary);
        FileError::Io(path.to_path_buf(), e)
    })
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unnamed".to_string());
    let mut temporary = path.to_path_buf();
    temporary.set_file_name(format!(".{name}.aitch-save"));
    temporary
}

/// Something went wrong reading or writing a file.
#[derive(Debug)]
pub enum FileError {
    Io(PathBuf, io::Error),
    Decode(String),
    Encode(String),
    /// A save was asked for on a buffer with no filename. Asking for one needs
    /// the prompt line, which is Phase 3.
    NoPath,
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileError::Io(path, e) => write!(f, "{}: {e}", path.display()),
            FileError::Decode(why) => write!(f, "cannot read this file: {why}"),
            FileError::Encode(why) => write!(f, "cannot write this file: {why}"),
            FileError::NoPath => write!(f, "this buffer has no filename yet"),
        }
    }
}

impl std::error::Error for FileError {}

#[cfg(test)]
mod tests {
    use super::*;

    // -- detection ---------------------------------------------------------

    #[test]
    fn a_bom_settles_it_immediately() {
        assert_eq!(
            detect(b"\xEF\xBB\xBFhi"),
            Encoding::new(Charset::Utf8, true)
        );
        assert_eq!(
            detect(b"\xFF\xFEh\0i\0"),
            Encoding::new(Charset::Utf16Le, true)
        );
        assert_eq!(
            detect(b"\xFE\xFF\0h\0i"),
            Encoding::new(Charset::Utf16Be, true)
        );
    }

    #[test]
    fn plain_ascii_and_utf8_need_no_bom() {
        assert_eq!(detect(b"hello world\n"), Encoding::UTF8);
        assert_eq!(detect("naïve café → 日本語\n".as_bytes()), Encoding::UTF8);
    }

    #[test]
    fn unmarked_utf16_is_not_mistaken_for_utf8() {
        // This is the trap: NUL is valid UTF-8, so `from_utf8` accepts this
        // happily and the file comes out full of NULs on the next save.
        let le: Vec<u8> = "hello, world\nsecond line\n"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert!(std::str::from_utf8(&le).is_ok(), "the trap is real");
        assert_eq!(detect(&le), Encoding::new(Charset::Utf16Le, false));

        let be: Vec<u8> = "hello, world\nsecond line\n"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        assert_eq!(detect(&be), Encoding::new(Charset::Utf16Be, false));
    }

    #[test]
    fn invalid_utf8_falls_back_to_latin1() {
        // 0xE9 is 'é' in Latin-1 and an incomplete sequence in UTF-8.
        let bytes = b"caf\xE9 na\xEFve\n";
        assert_eq!(detect(bytes), Encoding::new(Charset::Latin1, false));
    }

    #[test]
    fn a_stray_nul_does_not_make_a_file_utf16() {
        let mut bytes = b"a perfectly ordinary line of text\n".to_vec();
        bytes.push(0);
        bytes.extend_from_slice(b"and another one here\n");
        assert_eq!(detect(&bytes).charset, Charset::Utf8);
    }

    #[test]
    fn an_empty_file_is_utf8() {
        assert_eq!(detect(b""), Encoding::UTF8);
    }

    // -- round trips -------------------------------------------------------

    /// The property the whole file exists for: bytes in, bytes out.
    fn assert_round_trips(bytes: &[u8], expect: Charset, expect_bom: bool) {
        let loaded = load_bytes(bytes).expect("should load");
        assert_eq!(loaded.encoding.charset, expect, "charset");
        assert_eq!(loaded.encoding.bom, expect_bom, "bom");

        let written = encode(&loaded.text, loaded.encoding).expect("should encode");
        assert_eq!(written, bytes, "round trip changed the bytes");
    }

    #[test]
    fn utf8_round_trips() {
        assert_round_trips(b"hello\nworld\n", Charset::Utf8, false);
        assert_round_trips("naïve café → 日本語\n".as_bytes(), Charset::Utf8, false);
    }

    #[test]
    fn a_utf8_bom_survives_and_is_not_invented() {
        assert_round_trips(b"\xEF\xBB\xBFhello\n", Charset::Utf8, true);

        // And a file without one does not grow one.
        let written = encode("hello\n", Encoding::UTF8).unwrap();
        assert_eq!(written, b"hello\n");
    }

    #[test]
    fn utf16_round_trips_in_both_orders_and_both_bom_states() {
        let text = "hello\nworld\n";
        for (charset, bom_bytes) in [
            (Charset::Utf16Le, BOM_UTF16_LE),
            (Charset::Utf16Be, BOM_UTF16_BE),
        ] {
            let little = charset == Charset::Utf16Le;
            let body: Vec<u8> = text
                .encode_utf16()
                .flat_map(|u| {
                    if little {
                        u.to_le_bytes()
                    } else {
                        u.to_be_bytes()
                    }
                })
                .collect();

            assert_round_trips(&body, charset, false);

            let mut with_bom = bom_bytes.to_vec();
            with_bom.extend_from_slice(&body);
            assert_round_trips(&with_bom, charset, true);
        }
    }

    #[test]
    fn latin1_round_trips_byte_for_byte() {
        // Every byte value that is not valid UTF-8 on its own.
        let bytes: Vec<u8> = (0x80..=0xFFu8).collect();
        let loaded = load_bytes(&bytes).unwrap();
        assert_eq!(loaded.encoding.charset, Charset::Latin1);
        assert_eq!(encode(&loaded.text, loaded.encoding).unwrap(), bytes);
    }

    #[test]
    fn utf16_surrogate_pairs_survive() {
        // Outside the basic plane, so it needs two UTF-16 code units.
        let text = "music 𝄞 here\n";
        let body: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_round_trips(&body, Charset::Utf16Le, false);
    }

    // -- line endings ------------------------------------------------------

    #[test]
    fn line_endings_are_preserved_exactly_not_normalized() {
        for (bytes, dominant) in [
            (b"a\nb\nc\n".as_slice(), LineEnding::Lf),
            (b"a\r\nb\r\nc\r\n".as_slice(), LineEnding::CrLf),
            (b"a\rb\rc\r".as_slice(), LineEnding::Cr),
            // Mixed: every break must come back exactly as it went in.
            (b"a\r\nb\nc\r\n".as_slice(), LineEnding::CrLf),
        ] {
            let loaded = load_bytes(bytes).unwrap();
            assert_eq!(loaded.line_ending, dominant, "dominant ending");
            assert_eq!(
                encode(&loaded.text, loaded.encoding).unwrap(),
                bytes,
                "line endings were rewritten"
            );
        }
    }

    // -- failures ----------------------------------------------------------

    #[test]
    fn a_character_latin1_cannot_hold_is_refused_not_mangled() {
        let encoding = Encoding::new(Charset::Latin1, false);
        let error = encode("café 日本語", encoding).unwrap_err();
        assert!(error.to_string().contains("cannot be written as Latin-1"));
    }

    #[test]
    fn a_truncated_utf16_file_is_an_error() {
        let mut bytes = BOM_UTF16_LE.to_vec();
        bytes.extend_from_slice(&[0x68, 0x00, 0x69]); // one byte short
        let error = load_bytes(&bytes).unwrap_err();
        assert!(error.to_string().contains("odd number of bytes"));
    }

    // -- saving ------------------------------------------------------------

    #[test]
    fn saving_replaces_the_file_and_leaves_no_temporary_behind() {
        let dir = std::env::temp_dir().join(format!("aitch-save-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("round-trip.txt");

        fs::write(&path, b"original\r\ncontent\r\n").unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.line_ending, LineEnding::CrLf);

        // The acceptance criterion, in miniature: one edit, nothing else moves.
        let edited = loaded.text.replace("original", "edited");
        save(&path, &edited, loaded.encoding).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"edited\r\ncontent\r\n");

        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("aitch-save"))
            .collect();
        assert!(leftovers.is_empty(), "temporary file was left behind");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_failed_encode_leaves_the_original_file_alone() {
        let dir = std::env::temp_dir().join(format!("aitch-fail-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("latin1.txt");
        fs::write(&path, b"caf\xE9\n").unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.encoding.charset, Charset::Latin1);

        // Type a character Latin-1 cannot hold, then try to save.
        let edited = format!("{}日本語", loaded.text);
        assert!(save(&path, &edited, loaded.encoding).is_err());
        assert_eq!(
            fs::read(&path).unwrap(),
            b"caf\xE9\n",
            "the original was damaged by a failed save"
        );

        fs::remove_dir_all(&dir).ok();
    }
}
