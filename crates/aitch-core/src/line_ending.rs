//! Line endings: what a file uses, and what a new line should use.
//!
//! Existing line breaks are stored in the rope exactly as they were read and
//! are never rewritten. This is only about what a *newly typed* newline
//! inserts, so a CRLF file stays CRLF without a mixed file being normalized
//! behind the user's back.

use ropey::Rope;

/// The three line breaks in the wild.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// `\n`. Unix, and the default for a new file.
    #[default]
    Lf,
    /// `\r\n`. Windows.
    CrLf,
    /// `\r` alone. Classic Mac OS, and still found in old files.
    Cr,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
            LineEnding::Cr => "\r",
        }
    }

    /// The name shown to a user, matching what nano calls them.
    pub fn name(self) -> &'static str {
        match self {
            LineEnding::Lf => "Unix",
            LineEnding::CrLf => "DOS",
            LineEnding::Cr => "Mac",
        }
    }

    /// Length of the break in `char`s: 2 for CRLF, 1 for the others.
    ///
    /// Not `len`: this is not a collection, and calling it that invites the
    /// question of what an empty line ending would be.
    pub fn char_len(self) -> usize {
        match self {
            LineEnding::CrLf => 2,
            _ => 1,
        }
    }

    /// Which ending a text mostly uses.
    ///
    /// A file with no line break at all is Unix, because that is the sane
    /// default for whatever gets typed into it first.
    pub fn dominant(text: &Rope) -> LineEnding {
        let mut lf = 0usize;
        let mut crlf = 0usize;
        let mut cr = 0usize;

        let mut previous_was_cr = false;
        // Counting over the whole rope is O(n) but happens once, at load.
        for c in text.chars() {
            match c {
                '\r' => {
                    if previous_was_cr {
                        cr += 1;
                    }
                    previous_was_cr = true;
                }
                '\n' => {
                    if previous_was_cr {
                        crlf += 1;
                        previous_was_cr = false;
                    } else {
                        lf += 1;
                    }
                }
                _ => {
                    if previous_was_cr {
                        cr += 1;
                    }
                    previous_was_cr = false;
                }
            }
        }
        if previous_was_cr {
            cr += 1;
        }

        if crlf >= lf && crlf >= cr && crlf > 0 {
            LineEnding::CrLf
        } else if cr > lf && cr > crlf {
            LineEnding::Cr
        } else {
            LineEnding::Lf
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dominant(text: &str) -> LineEnding {
        LineEnding::dominant(&Rope::from_str(text))
    }

    #[test]
    fn an_empty_file_is_unix() {
        assert_eq!(dominant(""), LineEnding::Lf);
        assert_eq!(dominant("no line break here"), LineEnding::Lf);
    }

    #[test]
    fn each_ending_is_recognized_on_its_own() {
        assert_eq!(dominant("a\nb\nc\n"), LineEnding::Lf);
        assert_eq!(dominant("a\r\nb\r\nc\r\n"), LineEnding::CrLf);
        assert_eq!(dominant("a\rb\rc\r"), LineEnding::Cr);
    }

    #[test]
    fn a_lone_cr_is_not_mistaken_for_crlf() {
        assert_eq!(dominant("a\rb"), LineEnding::Cr);
        assert_eq!(dominant("a\r\nb"), LineEnding::CrLf);
    }

    #[test]
    fn the_majority_wins_in_a_mixed_file() {
        assert_eq!(dominant("a\r\nb\r\nc\n"), LineEnding::CrLf);
        assert_eq!(dominant("a\nb\nc\r\n"), LineEnding::Lf);
    }

    #[test]
    fn a_tie_goes_to_crlf() {
        // A file that is half DOS is a DOS file that someone edited badly;
        // typing Unix endings into it would only make the mixture worse.
        assert_eq!(dominant("a\r\nb\n"), LineEnding::CrLf);
    }

    #[test]
    fn lengths_and_text_agree() {
        for ending in [LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr] {
            assert_eq!(ending.as_str().chars().count(), ending.char_len());
        }
    }
}
