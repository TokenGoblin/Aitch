//! Benchmarks for the operations a large file makes hot.
//!
//! PLAN.md §6 sets these as budgets rather than aspirations. They are measured
//! here; enforcing them in CI needs a stored baseline, which is not wired yet.
//!
//! Run with `cargo bench -p aitch-core`.
//!
//! This is a hand-written harness, not criterion: `aitch-core` has zero
//! runtime or dev-dependencies (see CLAUDE.md / PLAN-ZERO-DEP.md), so there
//! is nothing here beyond `std::time::Instant` and plain arithmetic. It does
//! not do criterion's statistical sampling (outlier rejection, confidence
//! intervals) — it runs each closure enough times to get a stable mean and
//! prints that, plus a derived throughput figure for the two groups that
//! benefit from one. See `measure` below for the iteration strategy.

use std::hint::black_box;
use std::time::{Duration, Instant};

use aitch_core::{Buffer, Command, Highlighter, Language, PathIndex, Position, Viewport};

/// Run at least this many iterations, so a couple of lucky or unlucky
/// samples can't dominate the mean.
const MIN_ITERS: usize = 10;

/// Run for at least this long, so scheduler jitter and clock resolution
/// wash out. Matches the rough shape of criterion's own default measurement
/// window.
const MIN_DURATION: Duration = Duration::from_millis(500);

/// Never run more than this many iterations, regardless of how far under
/// `MIN_DURATION` each one lands. Without a cap, a microsecond-scale
/// closure (e.g. `reparse_after_a_keystroke`) would run hundreds of
/// thousands of times to fill 500ms; 10k iterations is already far more
/// samples than the mean needs to stabilize, and keeps the total suite
/// runtime bounded.
const MAX_ITERS: usize = 10_000;

/// The result of timing a closure: how long each call took on average, and
/// how many calls that average is over.
struct Timing {
    mean: Duration,
    iters: usize,
}

/// Times `f` by running it repeatedly until both `MIN_ITERS` and
/// `MIN_DURATION` are satisfied (whichever takes longer), or until
/// `MAX_ITERS` is hit — whichever comes first. This is a hybrid of "enough
/// samples" and "enough wall time": a benchmark whose closure is itself
/// expensive (e.g. `load`'s 50MB case) satisfies `MIN_DURATION` well before
/// `MIN_ITERS` would otherwise force it to keep reloading a huge buffer,
/// while a cheap closure keeps iterating until the clock, not the sample
/// count, says stop.
fn measure<F: FnMut()>(mut f: F) -> Timing {
    let start = Instant::now();
    let mut iters = 0usize;
    loop {
        f();
        iters += 1;
        let elapsed = start.elapsed();
        if (iters >= MIN_ITERS && elapsed >= MIN_DURATION) || iters >= MAX_ITERS {
            let mean = elapsed / iters as u32;
            return Timing { mean, iters };
        }
    }
}

/// Formats a duration the way a human skimming terminal output wants it:
/// milliseconds with a few decimal places, or microseconds for anything
/// under a millisecond.
fn fmt_duration(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1_000.0;
    if ms >= 1.0 {
        format!("{ms:.3} ms")
    } else {
        format!("{:.3} µs", ms * 1_000.0)
    }
}

/// Formats a bytes/sec figure in the largest unit that keeps the number
/// readable.
fn fmt_bytes_per_sec(bytes_per_sec: f64) -> String {
    const UNITS: [(&str, f64); 4] = [("GB/s", 1e9), ("MB/s", 1e6), ("KB/s", 1e3), ("B/s", 1.0)];
    for (unit, scale) in UNITS {
        if bytes_per_sec >= scale {
            return format!("{:.2} {unit}", bytes_per_sec / scale);
        }
    }
    format!("{bytes_per_sec:.2} B/s")
}

/// Formats an elements/sec figure, e.g. paths searched per second.
fn fmt_elements_per_sec(elements_per_sec: f64) -> String {
    if elements_per_sec >= 1_000_000.0 {
        format!("{:.2}M elem/s", elements_per_sec / 1_000_000.0)
    } else if elements_per_sec >= 1_000.0 {
        format!("{:.2}K elem/s", elements_per_sec / 1_000.0)
    } else {
        format!("{elements_per_sec:.2} elem/s")
    }
}

fn print_group(name: &str) {
    println!("\n{name}");
}

fn print_case(name: &str, timing: &Timing) {
    println!(
        "  {name}: {} ({} iters)",
        fmt_duration(timing.mean),
        timing.iters
    );
}

/// Like `print_case`, but also derives a bytes/sec figure from `bytes` (the
/// size of the input processed by one call of the timed closure).
fn print_case_bytes(name: &str, timing: &Timing, bytes: u64) {
    let bytes_per_sec = bytes as f64 / timing.mean.as_secs_f64();
    println!(
        "  {name}: {} ({} iters), {}",
        fmt_duration(timing.mean),
        timing.iters,
        fmt_bytes_per_sec(bytes_per_sec)
    );
}

/// Like `print_case`, but also derives an elements/sec figure from
/// `elements` (the number of items one call of the timed closure works
/// over).
fn print_case_elements(name: &str, timing: &Timing, elements: u64) {
    let elements_per_sec = elements as f64 / timing.mean.as_secs_f64();
    println!(
        "  {name}: {} ({} iters), {}",
        fmt_duration(timing.mean),
        timing.iters,
        fmt_elements_per_sec(elements_per_sec)
    );
}

/// A log-shaped file of roughly `megabytes` MB.
fn log_text(megabytes: usize) -> String {
    let target = megabytes * 1024 * 1024;
    let mut text = String::with_capacity(target + 128);
    let mut i = 0usize;
    while text.len() < target {
        text.push_str(&format!(
            "2026-09-08T12:34:56.{:03}Z [INFO ] worker-{:02} segment id={i} latency={}.{:03}ms\n",
            i % 1000,
            i % 32,
            i % 97,
            i % 1000
        ));
        i += 1;
    }
    text
}

fn load() {
    print_group("load");
    for megabytes in [1usize, 8, 50] {
        let text = log_text(megabytes);
        let bytes = text.len() as u64;
        let timing = measure(|| {
            let buffer = Buffer::from_reader(black_box(text.as_bytes())).unwrap();
            black_box(buffer.len_lines());
        });
        print_case_bytes(&format!("{megabytes}MB"), &timing, bytes);
    }
}

/// Reading the visible window is what every frame does, and it has to stay
/// independent of how far into the file the window sits.
fn visible_window() {
    let buffer = Buffer::from_str(&log_text(50));
    let lines = buffer.len_lines();

    print_group("visible_window");
    for (name, first) in [
        ("at_start", 0usize),
        ("at_middle", lines / 2),
        ("at_end", lines.saturating_sub(60)),
    ] {
        let timing = measure(|| {
            let mut total = 0usize;
            for line in first..(first + 50).min(lines) {
                total += buffer.line_text(black_box(line)).len_chars();
            }
            black_box(total);
        });
        print_case(name, &timing);
    }
}

fn movement() {
    let text = log_text(50);
    let viewport = Viewport::new(50);

    print_group("movement");

    {
        let mut buffer = Buffer::from_str(&text);
        let timing = measure(|| {
            for _ in 0..100 {
                buffer.apply(black_box(&Command::MoveDown), &viewport);
            }
        });
        print_case("down_100_lines", &timing);
    }

    {
        let mut buffer = Buffer::from_str(&text);
        let timing = measure(|| {
            for _ in 0..100 {
                buffer.apply(black_box(&Command::MovePageDown), &viewport);
            }
        });
        print_case("page_down_100_screens", &timing);
    }

    // Jumping to the end of a 50 MB rope must not walk it.
    {
        let mut buffer = Buffer::from_str(&text);
        let timing = measure(|| {
            buffer.apply(black_box(&Command::MoveBufferStart), &viewport);
            buffer.apply(black_box(&Command::MoveBufferEnd), &viewport);
        });
        print_case("to_buffer_end", &timing);
    }

    {
        let mut buffer = Buffer::from_str(&text);
        let lines = buffer.len_lines();
        let mut i = 0usize;
        let timing = measure(|| {
            // A cheap spread over the file without a random dependency.
            i = i.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let line = (i >> 33) % lines;
            black_box(buffer.set_cursor(black_box(Position::new(line, 0))));
        });
        print_case("set_cursor_random_line", &timing);
    }
}

/// A path list the shape of a large checkout, without needing one on disk.
fn synthetic_paths(count: usize) -> Vec<String> {
    let areas = ["drivers", "arch", "kernel", "fs", "net", "sound", "tools"];
    let kinds = ["core", "init", "probe", "ioctl", "debug", "table", "queue"];
    (0..count)
        .map(|i| {
            format!(
                "{}/{}{}/{}_{}.c",
                areas[i % areas.len()],
                kinds[i % kinds.len()],
                i % 400,
                kinds[(i / 7) % kinds.len()],
                i
            )
        })
        .collect()
}

/// PLAN.md Phase 4: quick open filters 80k paths with no perceptible lag.
/// The walk that builds the index is separate and happens once; this is the
/// half that runs on every keystroke.
fn quick_open() {
    let index = PathIndex::from_paths(
        std::path::PathBuf::from("/project"),
        synthetic_paths(80_000),
    );
    let elements = index.len() as u64;

    print_group("quick_open");
    for query in ["d", "dr", "drv", "driverscore", "netqueue42"] {
        let timing = measure(|| {
            black_box(index.search(black_box(query), 50).len());
        });
        print_case_elements(query, &timing, elements);
    }
}

/// Roughly `lines` lines of ordinary Rust, for the frame-budget bench.
fn rust_source(lines: usize) -> String {
    let mut source = String::from("use std::collections::HashMap;\n\n");
    let mut written = 2;
    let mut n = 0;
    while written < lines {
        source.push_str(&format!(
            "/// Does the {n}th thing.\n\
             pub fn thing_{n}(input: &str, count: usize) -> HashMap<String, usize> {{\n\
             \x20   let mut out = HashMap::new();\n\
             \x20   for (index, word) in input.split_whitespace().enumerate() {{\n\
             \x20       if index < count {{\n\
             \x20           out.insert(word.to_string(), index * {n});\n\
             \x20       }}\n\
             \x20   }}\n\
             \x20   out\n\
             }}\n\n"
        ));
        written += 11;
        n += 1;
    }
    source
}

/// PLAN.md Phase 5: typing in a 10k-line Rust file stays inside a 16 ms frame
/// with highlighting on.
///
/// Parsing runs off the UI thread, so what the frame actually pays for is the
/// keystroke and the span lookup. Both are measured here; the parse is
/// measured too, because if it cannot keep up the colour lags behind the text
/// even though the frames stay smooth.
fn highlighting() {
    let source = rust_source(10_000);
    let buffer = Buffer::from_str(&source);
    let lines = buffer.snapshot_lines();
    assert!(
        lines.len() >= 10_000,
        "wanted 10k lines, built {}",
        lines.len()
    );

    print_group("highlighting");

    // A first lex, which happens once when a file is opened.
    {
        let timing = measure(|| {
            let mut highlighter = Highlighter::new(Language::Rust).unwrap();
            highlighter.parse(black_box(&lines), &[]);
        });
        print_case("first_parse_10k_lines", &timing);
    }

    // A re-lex after one character, which is what typing costs.
    {
        let mut highlighter = Highlighter::new(Language::Rust).unwrap();
        highlighter.parse(&lines, &[]);
        let middle_line = lines.len() / 2;
        let edit = aitch_core::TextEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 0,
            start_point: (middle_line, 0),
            old_end_point: (middle_line, 0),
            new_end_point: (middle_line, 0),
        };

        let timing = measure(|| {
            highlighter.parse(black_box(&lines), std::slice::from_ref(&edit));
        });
        print_case("reparse_after_a_keystroke", &timing);
    }

    // The spans for one screen of text, which the renderer waits on.
    {
        let mut highlighter = Highlighter::new(Language::Rust).unwrap();
        highlighter.parse(&lines, &[]);
        let start = buffer.line_to_byte(5_000);
        let end = buffer.line_to_byte(5_060);

        let timing = measure(|| {
            black_box(highlighter.spans(start..end).len());
        });
        print_case("spans_for_one_screen", &timing);
    }

    // What the UI thread itself pays per keystroke: the edit, and handing a
    // snapshot to the lexer thread. Nothing here may approach 16 ms.
    {
        let mut buffer = Buffer::from_str(&source);
        buffer.set_cursor(Position::new(5_000, 0));
        let viewport = Viewport::new(50);

        let timing = measure(|| {
            buffer.apply(&Command::InsertText("x".to_string()), &viewport);
            let edits = buffer.take_text_edits();
            // What gets handed to the lexer thread.
            let snapshot = buffer.snapshot_lines();
            black_box((edits.len(), snapshot.len()));
        });
        print_case("keystroke_on_the_ui_thread", &timing);
    }
}

fn main() {
    load();
    visible_window();
    movement();
    quick_open();
    highlighting();
    println!();
}
