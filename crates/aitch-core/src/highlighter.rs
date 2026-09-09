//! Parsing off the UI thread.
//!
//! PLAN.md Phase 5 puts the frame budget at 16 ms with highlighting on, and
//! the way to keep it is not to make parsing fast enough — it is to keep
//! parsing off the thread that draws. The editor hands over a snapshot and
//! carries on; colour arrives when it arrives, and a frame drawn a keystroke
//! behind on colour is far better than a frame that waits.
//!
//! Two details make that work:
//!
//! - **Snapshots are free.** A [`Rope`] clone shares its structure, so handing
//!   the worker the whole document costs nothing and it never sees a half-
//!   applied edit.
//! - **Stale work is dropped, not queued.** Typing faster than the parser runs
//!   would otherwise build a backlog of answers nobody wants any more. Only
//!   the newest request is worked on — but its *edits* are the accumulated
//!   edits of everything it superseded, because tree-sitter adjusts its old
//!   tree by replaying them and a gap makes it reuse the wrong subtrees.

use std::ops::Range;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use ropey::Rope;
use tree_sitter::{InputEdit, Point};

use crate::buffer::TextEdit;
use crate::syntax::{Highlighter, Language, Span};

/// A request to highlight a snapshot.
struct Job {
    text: Rope,
    edits: Vec<TextEdit>,
    range: Range<usize>,
    generation: u64,
    /// A different document from the last one: throw the tree away first.
    ///
    /// The worker keeps its tree between requests, which is the whole point of
    /// an incremental parser. But a buffer switch between two files of the
    /// same language keeps the same worker, and then the edits say nothing
    /// changed — so tree-sitter reused the previous file's tree wholesale and
    /// coloured the new file at the old file's offsets.
    fresh: bool,
}

impl Job {
    /// Replace this request with a newer one, keeping both sets of edits.
    ///
    /// The snapshot, range and generation are the newer one's: that is the
    /// document the answer will describe. The edits are not. They are how the
    /// text the parser last saw became this text, and tree-sitter replays them
    /// against its existing tree before reparsing. Dropping the superseded
    /// job's edits leaves that tree adjusted for a document that never
    /// existed, and it then reuses subtrees at the wrong offsets — a comment
    /// typed at the top of a file comes back uncoloured until the next
    /// keystroke happens to land a job that is not coalesced.
    fn superseded_by(self, newer: Job) -> Job {
        let mut edits = self.edits;
        edits.extend(newer.edits);
        Job {
            text: newer.text,
            edits,
            range: newer.range,
            generation: newer.generation,
            // If either asked for a clean parse, the tree the older one was
            // written against is gone regardless, so the merged job wants one.
            fresh: self.fresh || newer.fresh,
        }
    }
}

/// Coloured spans for one snapshot of the document.
#[derive(Debug, Clone, Default)]
pub struct Highlights {
    /// Which snapshot these describe. Anything older than the buffer's
    /// current generation is out of date and drawn only until better arrives.
    pub generation: u64,
    /// The byte range they cover; outside it there is simply no answer yet.
    pub range: Range<usize>,
    pub spans: Vec<Span>,
}

impl Highlights {
    /// The token covering a byte offset, if any.
    ///
    /// Spans are ordered and non-overlapping, so this is a binary search.
    pub fn token_at(&self, byte: usize) -> Option<crate::syntax::Token> {
        let index = self
            .spans
            .binary_search_by(|span| {
                if span.end <= byte {
                    std::cmp::Ordering::Less
                } else if span.start > byte {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .ok()?;
        Some(self.spans[index].token)
    }
}

/// A worker thread holding a parser for one language.
pub struct SyntaxThread {
    jobs: Sender<Job>,
    results: Receiver<Highlights>,
    language: Language,
    /// Bumped for each request, so an answer can be matched to its snapshot.
    generation: u64,
    /// The most recent answer received.
    highlights: Highlights,
}

impl SyntaxThread {
    /// Start a parser for `language`, calling `on_ready` when an answer lands.
    ///
    /// `on_ready` exists to wake an event loop that is otherwise asleep; the
    /// core cannot know how, so the caller says. Returns `None` if the grammar
    /// will not load, in which case the file is shown without colour.
    pub fn new<F>(language: Language, on_ready: F) -> Option<SyntaxThread>
    where
        F: Fn() + Send + 'static,
    {
        // Fail here rather than on the worker, where nobody would see it.
        let mut highlighter = Highlighter::new(language)?;

        let (jobs, incoming) = mpsc::channel::<Job>();
        let (finished, results) = mpsc::channel::<Highlights>();

        std::thread::Builder::new()
            .name(format!("aitch-syntax-{}", language.name()))
            .spawn(move || {
                while let Ok(mut job) = incoming.recv() {
                    // Take the newest request and throw the rest away: an
                    // answer about a document three keystrokes ago is of no
                    // use to anyone.
                    loop {
                        match incoming.try_recv() {
                            Ok(newer) => job = job.superseded_by(newer),
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => return,
                        }
                    }

                    // A different document: the tree describes the old one,
                    // and the edits between them do not exist.
                    if job.fresh {
                        highlighter.forget();
                    } else {
                        for edit in &job.edits {
                            highlighter.edit(&input_edit(edit));
                        }
                    }
                    highlighter.parse(&job.text);

                    let spans = highlighter.spans(&job.text, job.range.clone());
                    let answer = Highlights {
                        generation: job.generation,
                        range: job.range,
                        spans,
                    };
                    if finished.send(answer).is_err() {
                        return;
                    }
                    on_ready();
                }
            })
            .ok()?;

        Some(SyntaxThread {
            jobs,
            results,
            language,
            generation: 0,
            highlights: Highlights::default(),
        })
    }

    pub fn language(&self) -> Language {
        self.language
    }

    /// The most recent answer. May describe a slightly older document.
    pub fn highlights(&self) -> &Highlights {
        &self.highlights
    }

    /// Ask for the visible range of a snapshot to be highlighted.
    ///
    /// Cheap: a rope clone shares its structure, so this copies no text.
    /// Ask for colour. `fresh` means this is a different document from the
    /// last request, so nothing the parser is holding applies to it.
    pub fn request(&mut self, text: &Rope, edits: Vec<TextEdit>, range: Range<usize>, fresh: bool) {
        self.generation += 1;
        let job = Job {
            text: text.clone(),
            edits,
            range,
            generation: self.generation,
            fresh,
        };
        // A dead worker means no colour, which is survivable and already how
        // an unknown language behaves.
        let _ = self.jobs.send(job);
    }

    /// Collect whatever the worker has finished. True if anything arrived.
    pub fn poll(&mut self) -> bool {
        let mut arrived = false;
        while let Ok(highlights) = self.results.try_recv() {
            // Answers can only be superseded, never un-answered.
            if highlights.generation >= self.highlights.generation {
                self.highlights = highlights;
                arrived = true;
            }
        }
        arrived
    }

    /// Whether the newest answer describes the newest request.
    pub fn is_current(&self) -> bool {
        self.highlights.generation == self.generation
    }
}

impl std::fmt::Debug for SyntaxThread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SyntaxThread({})", self.language.name())
    }
}

fn input_edit(edit: &TextEdit) -> InputEdit {
    InputEdit {
        start_byte: edit.start_byte,
        old_end_byte: edit.old_end_byte,
        new_end_byte: edit.new_end_byte,
        start_position: Point::new(edit.start_point.0, edit.start_point.1),
        old_end_position: Point::new(edit.old_end_point.0, edit.old_end_point.1),
        new_end_position: Point::new(edit.new_end_point.0, edit.new_end_point.1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::Token;
    use crate::Buffer;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// Wait for the worker to answer. Threads are not instant.
    fn settle(thread: &mut SyntaxThread) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            thread.poll();
            if thread.is_current() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn a_snapshot_comes_back_highlighted() {
        let mut thread = SyntaxThread::new(Language::Rust, || {}).expect("a worker");
        let text = Rope::from_str("fn main() { let x = 1; }\n");

        thread.request(&text, Vec::new(), 0..text.len_bytes(), true);
        assert!(settle(&mut thread), "the worker never answered");

        let highlights = thread.highlights();
        assert!(!highlights.spans.is_empty());
        assert_eq!(highlights.token_at(0), Some(Token::Keyword), "fn");
    }

    #[test]
    fn the_caller_is_told_when_an_answer_lands() {
        let woken = Arc::new(AtomicUsize::new(0));
        let counter = woken.clone();
        let mut thread = SyntaxThread::new(Language::Rust, move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("a worker");

        let text = Rope::from_str("fn main() {}\n");
        thread.request(&text, Vec::new(), 0..text.len_bytes(), true);
        assert!(settle(&mut thread));
        assert!(woken.load(Ordering::SeqCst) > 0, "the loop was never woken");
    }

    #[test]
    fn typing_faster_than_the_parser_does_not_build_a_backlog() {
        let mut thread = SyntaxThread::new(Language::Rust, || {}).expect("a worker");

        // Fifty snapshots in a row, as a fast typist would produce.
        let mut source = String::from("fn main() {\n");
        for i in 0..50 {
            source.push_str(&format!("    let x{i} = {i};\n"));
            let text = Rope::from_str(&format!("{source}}}\n"));
            thread.request(&text, Vec::new(), 0..text.len_bytes(), true);
        }

        assert!(settle(&mut thread), "the worker never caught up");
        // What matters is that the answer describes the newest request, not
        // that fifty answers came back.
        assert!(thread.is_current());
        assert!(!thread.highlights().spans.is_empty());
    }

    #[test]
    fn an_incremental_edit_gives_the_same_answer_as_a_fresh_parse() {
        // The buffer's byte-level edits are the thing under test: if they are
        // wrong, tree-sitter reuses the wrong parts of the tree and the colour
        // silently drifts from the text.
        let mut buffer = Buffer::from_str("fn main() { let x = 1; }\n");
        let mut incremental = SyntaxThread::new(Language::Rust, || {}).expect("a worker");
        // A document this worker has not seen, so nothing to reuse.
        incremental.request(
            &buffer.text().clone(),
            Vec::new(),
            0..buffer.text().len_bytes(),
            true,
        );
        assert!(settle(&mut incremental));

        buffer.set_cursor(crate::Position::new(0, 17));
        buffer.insert("yz");
        let edits = buffer.take_text_edits();
        assert!(!edits.is_empty(), "the buffer reported no change");

        let text = buffer.text().clone();
        // Not fresh: the same document, edited. Reusing the tree here is
        // exactly what this test exists to check.
        incremental.request(&text, edits, 0..text.len_bytes(), false);
        assert!(settle(&mut incremental));

        let mut fresh = SyntaxThread::new(Language::Rust, || {}).expect("a worker");
        fresh.request(&text, Vec::new(), 0..text.len_bytes(), true);
        assert!(settle(&mut fresh));

        assert_eq!(
            incremental.highlights().spans,
            fresh.highlights().spans,
            "incremental parsing drifted from a fresh parse"
        );
    }

    #[test]
    fn a_token_can_be_found_by_byte_offset() {
        let mut thread = SyntaxThread::new(Language::Rust, || {}).expect("a worker");
        let source = "// note\nfn main() {}\n";
        let text = Rope::from_str(source);
        thread.request(&text, Vec::new(), 0..text.len_bytes(), true);
        assert!(settle(&mut thread));

        let highlights = thread.highlights();
        assert_eq!(highlights.token_at(2), Some(Token::Comment), "inside //");
        assert_eq!(highlights.token_at(8), Some(Token::Keyword), "fn");
    }

    #[test]
    fn coalescing_two_requests_keeps_both_sets_of_edits() {
        // The parser replays these against the tree it already has. Keeping
        // only the newer job's edits leaves a gap, and tree-sitter then reuses
        // subtrees at offsets that no longer mean anything.
        let older = Job {
            text: Rope::from_str("ab"),
            edits: vec![edit_at(0), edit_at(1)],
            range: 0..2,
            generation: 1,
            fresh: false,
        };
        let newer = Job {
            text: Rope::from_str("abcd"),
            edits: vec![edit_at(2), edit_at(3)],
            range: 0..4,
            generation: 2,
            fresh: false,
        };

        let merged = older.superseded_by(newer);
        assert_eq!(merged.generation, 2, "the newer snapshot is the answer");
        assert_eq!(merged.text.to_string(), "abcd");
        assert_eq!(merged.range, 0..4);
        let starts: Vec<usize> = merged.edits.iter().map(|e| e.start_byte).collect();
        assert_eq!(starts, vec![0, 1, 2, 3], "in order, and none lost");
        assert!(!merged.fresh, "neither asked for a clean parse");
    }

    #[test]
    fn coalescing_keeps_a_request_for_a_clean_parse() {
        // A buffer switch asks for the tree to be thrown away. If that is
        // merged with a later keystroke's job and lost, the new document is
        // parsed against the old document's tree -- which is the bug the flag
        // exists to stop, reappearing only when typing is fast enough to
        // coalesce.
        let switched = Job {
            text: Rope::from_str("ab"),
            edits: Vec::new(),
            range: 0..2,
            generation: 1,
            fresh: true,
        };
        let typed = Job {
            text: Rope::from_str("abc"),
            edits: vec![edit_at(2)],
            range: 0..3,
            generation: 2,
            fresh: false,
        };

        assert!(switched.superseded_by(typed).fresh, "the reset survives");
    }

    /// A one-character insertion at `byte`, which is all these tests need.
    fn edit_at(byte: usize) -> TextEdit {
        TextEdit {
            start_byte: byte,
            old_end_byte: byte,
            new_end_byte: byte + 1,
            start_point: (0, byte),
            old_end_point: (0, byte),
            new_end_point: (0, byte + 1),
        }
    }

    #[test]
    fn typing_a_comment_without_pausing_still_colours_it() {
        // Every keystroke sends a request, and the worker coalesces whatever
        // has piled up. The answer has to describe the text as typed however
        // many of those requests were merged on the way.
        let mut buffer = Buffer::from_str("fn main() {}\n");
        let mut thread = SyntaxThread::new(Language::Rust, || {}).expect("a worker");
        thread.request(
            &buffer.text().clone(),
            Vec::new(),
            0..buffer.text().len_bytes(),
            true,
        );
        assert!(settle(&mut thread));

        buffer.set_cursor(crate::Position::new(0, 0));
        for character in "// note\n".chars() {
            if character == '\n' {
                buffer.insert("\n");
            } else {
                buffer.insert(&character.to_string());
            }
            let text = buffer.text().clone();
            let edits = buffer.take_text_edits();
            thread.request(&text, edits, 0..text.len_bytes(), false);
        }

        assert!(settle(&mut thread), "the worker never caught up");
        assert_eq!(
            thread.highlights().token_at(2),
            Some(Token::Comment),
            "the comment that was just typed came back uncoloured"
        );
    }

    #[test]
    fn no_answer_yet_is_not_an_error() {
        let thread = SyntaxThread::new(Language::Rust, || {}).expect("a worker");
        assert!(thread.highlights().spans.is_empty());
        assert!(thread.highlights().token_at(0).is_none());
    }
}
