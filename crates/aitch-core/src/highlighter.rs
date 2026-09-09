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
//!   the newest request is worked on.

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
                            Ok(newer) => job = newer,
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => return,
                        }
                    }

                    for edit in &job.edits {
                        highlighter.edit(&input_edit(edit));
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
    pub fn request(&mut self, text: &Rope, edits: Vec<TextEdit>, range: Range<usize>) {
        self.generation += 1;
        let job = Job {
            text: text.clone(),
            edits,
            range,
            generation: self.generation,
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

        thread.request(&text, Vec::new(), 0..text.len_bytes());
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
        thread.request(&text, Vec::new(), 0..text.len_bytes());
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
            thread.request(&text, Vec::new(), 0..text.len_bytes());
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
        incremental.request(
            &buffer.text().clone(),
            Vec::new(),
            0..buffer.text().len_bytes(),
        );
        assert!(settle(&mut incremental));

        buffer.set_cursor(crate::Position::new(0, 17));
        buffer.insert("yz");
        let edits = buffer.take_text_edits();
        assert!(!edits.is_empty(), "the buffer reported no change");

        let text = buffer.text().clone();
        incremental.request(&text, edits, 0..text.len_bytes());
        assert!(settle(&mut incremental));

        let mut fresh = SyntaxThread::new(Language::Rust, || {}).expect("a worker");
        fresh.request(&text, Vec::new(), 0..text.len_bytes());
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
        thread.request(&text, Vec::new(), 0..text.len_bytes());
        assert!(settle(&mut thread));

        let highlights = thread.highlights();
        assert_eq!(highlights.token_at(2), Some(Token::Comment), "inside //");
        assert_eq!(highlights.token_at(8), Some(Token::Keyword), "fn");
    }

    #[test]
    fn no_answer_yet_is_not_an_error() {
        let thread = SyntaxThread::new(Language::Rust, || {}).expect("a worker");
        assert!(thread.highlights().spans.is_empty());
        assert!(thread.highlights().token_at(0).is_none());
    }
}
