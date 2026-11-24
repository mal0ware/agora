//! Criterion benchmarks on synthetic streams.
//!
//! These say how the code compares with itself from one change to the next.
//! The full-day numbers in the README come from `agora reproduce`, not from
//! here.

use std::hint::black_box;

use agora::book::{Book, Handle, Side};
use agora::engine::Engine;
use agora::itch::{parse, FrameReader};
use agora::replay::Replay;
use agora::synth::{generate, SynthConfig};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};

const CFG: SynthConfig = SynthConfig {
    seed: 7,
    messages: 200_000,
    symbols: 16,
};

fn frames(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let len = usize::from(u16::from_be_bytes([bytes[at], bytes[at + 1]]));
        out.push(&bytes[at + 2..at + 2 + len]);
        at += 2 + len;
    }
    out
}

fn bench_parse(c: &mut Criterion) {
    let synth = generate(&CFG);
    let frames = frames(&synth.bytes);
    let mut group = c.benchmark_group("parse");
    group.throughput(Throughput::Elements(frames.len() as u64));
    group.bench_function("synthetic_stream", |b| {
        b.iter(|| {
            let mut sink = 0u64;
            for f in &frames {
                sink = sink.wrapping_add(parse(black_box(f)).unwrap().header.timestamp);
            }
            sink
        })
    });
    group.finish();
}

fn bench_replay(c: &mut Criterion) {
    let synth = generate(&CFG);
    let mut group = c.benchmark_group("replay");
    group.throughput(Throughput::Elements(synth.messages));
    for (name, check) in [("unchecked", false), ("checked", true)] {
        group.bench_function(name, |b| {
            b.iter(|| {
                let mut replay = Replay::new(check);
                let mut reader = FrameReader::new(&synth.bytes[..]);
                while let Some(frame) = reader.next_frame().unwrap() {
                    replay.apply(&parse(frame).unwrap());
                }
                replay.live_orders()
            })
        });
    }
    group.finish();
}

fn deep_book() -> (Book, Vec<Handle>) {
    let mut book = Book::new();
    let mut slots = Vec::new();
    for level in 0..200u32 {
        for k in 0..5u64 {
            let id = u64::from(level) * 10 + k;
            slots.push(book.add(id, Side::Buy, 10_000 - level, 100).unwrap());
            slots.push(book.add(id + 5, Side::Sell, 10_003 + level, 100).unwrap());
        }
    }
    (book, slots)
}

fn bench_book(c: &mut Criterion) {
    let mut group = c.benchmark_group("book");
    group.bench_function("add_then_remove_at_best", |b| {
        let (mut book, _) = deep_book();
        b.iter(|| {
            let slot = book
                .add(u64::MAX, Side::Buy, black_box(10_000), 100)
                .unwrap();
            book.remove(slot)
        })
    });
    group.bench_function("add_then_remove_150_levels_deep", |b| {
        let (mut book, _) = deep_book();
        b.iter(|| {
            let slot = book
                .add(u64::MAX, Side::Buy, black_box(9_850), 100)
                .unwrap();
            book.remove(slot)
        })
    });
    group.bench_function("new_level_inside_spread_then_remove", |b| {
        let (mut book, _) = deep_book();
        b.iter(|| {
            let slot = book
                .add(u64::MAX, Side::Sell, black_box(10_001), 100)
                .unwrap();
            book.remove(slot)
        })
    });
    group.bench_function("partial_reduce", |b| {
        b.iter_batched_ref(
            deep_book,
            |(book, slots)| {
                for &s in slots.iter() {
                    black_box(book.reduce(s, 1).unwrap());
                }
            },
            BatchSize::LargeInput,
        )
    });
    group.finish();
}

fn bench_engine(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine");
    group.bench_function("sweep_five_orders_and_refill", |b| {
        let mut engine = Engine::new();
        let mut fills = Vec::new();
        let mut id = 0u64;
        for _ in 0..5 {
            id += 1;
            engine
                .submit(id, Side::Sell, 10_001, 100, &mut fills)
                .unwrap();
        }
        b.iter(|| {
            fills.clear();
            id += 1;
            engine
                .submit(id, Side::Buy, 10_001, 500, &mut fills)
                .unwrap();
            for _ in 0..5 {
                id += 1;
                engine
                    .submit(id, Side::Sell, 10_001, 100, &mut fills)
                    .unwrap();
            }
            fills.len()
        })
    });
    group.finish();
}

criterion_group!(benches, bench_parse, bench_replay, bench_book, bench_engine);
criterion_main!(benches);
