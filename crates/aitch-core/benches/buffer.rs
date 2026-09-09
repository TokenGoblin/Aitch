//! Benchmarks for the operations a large file makes hot.
//!
//! PLAN.md §6 sets these as budgets rather than aspirations. They are measured
//! here; enforcing them in CI needs a stored baseline, which is not wired yet.
//!
//! Run with `cargo bench -p aitch-core`.

use std::hint::black_box;

use aitch_core::{Buffer, Command, PathIndex, Position, Viewport};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

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

fn load(c: &mut Criterion) {
    let mut group = c.benchmark_group("load");
    for megabytes in [1usize, 8, 50] {
        let text = log_text(megabytes);
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{megabytes}MB")),
            &text,
            |b, text| {
                b.iter(|| {
                    let buffer = Buffer::from_reader(black_box(text.as_bytes())).unwrap();
                    black_box(buffer.len_lines())
                })
            },
        );
    }
    group.finish();
}

/// Reading the visible window is what every frame does, and it has to stay
/// independent of how far into the file the window sits.
fn visible_window(c: &mut Criterion) {
    let buffer = Buffer::from_str(&log_text(50));
    let lines = buffer.len_lines();

    let mut group = c.benchmark_group("visible_window");
    for (name, first) in [
        ("at_start", 0usize),
        ("at_middle", lines / 2),
        ("at_end", lines.saturating_sub(60)),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| {
                let mut total = 0usize;
                for line in first..(first + 50).min(lines) {
                    total += buffer.line_text(black_box(line)).len_chars();
                }
                black_box(total)
            })
        });
    }
    group.finish();
}

fn movement(c: &mut Criterion) {
    let text = log_text(50);
    let viewport = Viewport::new(50);

    let mut group = c.benchmark_group("movement");

    group.bench_function("down_100_lines", |b| {
        let mut buffer = Buffer::from_str(&text);
        b.iter(|| {
            for _ in 0..100 {
                buffer.apply(black_box(&Command::MoveDown), &viewport);
            }
        })
    });

    group.bench_function("page_down_100_screens", |b| {
        let mut buffer = Buffer::from_str(&text);
        b.iter(|| {
            for _ in 0..100 {
                buffer.apply(black_box(&Command::MovePageDown), &viewport);
            }
        })
    });

    // Jumping to the end of a 50 MB rope must not walk it.
    group.bench_function("to_buffer_end", |b| {
        let mut buffer = Buffer::from_str(&text);
        b.iter(|| {
            buffer.apply(black_box(&Command::MoveBufferStart), &viewport);
            buffer.apply(black_box(&Command::MoveBufferEnd), &viewport);
        })
    });

    group.bench_function("set_cursor_random_line", |b| {
        let mut buffer = Buffer::from_str(&text);
        let lines = buffer.len_lines();
        let mut i = 0usize;
        b.iter(|| {
            // A cheap spread over the file without a random dependency.
            i = i.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let line = (i >> 33) % lines;
            buffer.set_cursor(black_box(Position::new(line, 0)))
        })
    });

    group.finish();
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
fn quick_open(c: &mut Criterion) {
    let index = PathIndex::from_paths(
        std::path::PathBuf::from("/project"),
        synthetic_paths(80_000),
    );

    let mut group = c.benchmark_group("quick_open");
    group.throughput(Throughput::Elements(index.len() as u64));
    for query in ["d", "dr", "drv", "driverscore", "netqueue42"] {
        group.bench_with_input(BenchmarkId::from_parameter(query), query, |b, query| {
            b.iter(|| black_box(index.search(black_box(query), 50).len()))
        });
    }
    group.finish();
}

criterion_group!(benches, load, visible_window, movement, quick_open);
criterion_main!(benches);
