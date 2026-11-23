//! Golden tests: fixed inputs, fixed fingerprints.
//!
//! The design called for a golden test on the first million messages of a
//! real sample day. That file cannot be committed, so there are three tests:
//!
//! 1. the committed synthetic stream (`tests/data/synthetic.itch`) against a
//!    pinned digest, which runs everywhere;
//! 2. one million generated messages against a pinned digest, which runs
//!    everywhere;
//! 3. the first million messages of the real day against a pinned digest,
//!    which runs only where `data/12302019.NASDAQ_ITCH50` exists.
//!
//! A fourth test runs the smoke reproduce command twice and compares its
//! output with the committed `results/smoke/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use agora::itch::{parse, FrameReader};
use agora::replay::{Audit, Replay};
use agora::synth::{generate, SynthConfig};

/// Must match `SMOKE` in `src/main.rs`.
const SMOKE: SynthConfig = SynthConfig {
    seed: 20_191_230,
    messages: 50_000,
    symbols: 8,
};
const SMOKE_DIGEST: u64 = 0x0225_ce2c_2d83_69b2;

const MILLION: SynthConfig = SynthConfig {
    seed: 5,
    messages: 1_000_000,
    symbols: 32,
};
const MILLION_DIGEST: u64 = 0x1a65_3aca_79c3_6eed;
const MILLION_RESTING_ORDERS: u64 = 67_712;

const REAL_DAY: &str = "data/12302019.NASDAQ_ITCH50";
const REAL_FIRST_MILLION_DIGEST: u64 = 0x8b54_6eda_1fe3_7dee;
const REAL_FIRST_MILLION_RESTING_ORDERS: u64 = 5_604;

fn replay(reader: impl std::io::Read, limit: u64) -> (Replay, Audit) {
    let mut replay = Replay::new(true);
    let mut frames = FrameReader::new(reader);
    let mut n = 0;
    while n < limit {
        let Some(frame) = frames.next_frame().unwrap() else {
            break;
        };
        replay.apply(&parse(frame).unwrap());
        n += 1;
    }
    let audit = replay.finish();
    (replay, audit)
}

#[test]
fn committed_synthetic_stream() {
    let bytes = fs::read("tests/data/synthetic.itch").unwrap();
    let synth = generate(&SMOKE);
    assert!(
        bytes == synth.bytes,
        "tests/data/synthetic.itch is not what the generator makes"
    );
    assert_eq!(synth.digest, SMOKE_DIGEST);
    let (replay, audit) = replay(&bytes[..], u64::MAX);
    assert_eq!(replay.stats.messages, 50_000);
    assert_eq!(audit.digest, SMOKE_DIGEST);
    assert!(audit.conserved());
    assert_eq!(replay.stats.crossed_updates, 0);
    // Every one of the 23 message types is present.
    for (t, name, _, _) in agora::itch::TYPES {
        assert!(replay.stats.counts[usize::from(t)] > 0, "no {name} message");
    }
}

#[test]
fn one_million_generated_messages() {
    let synth = generate(&MILLION);
    assert_eq!(synth.messages, 1_000_000);
    let (replay, audit) = replay(&synth.bytes[..], u64::MAX);
    assert_eq!(audit.digest, synth.digest);
    assert_eq!(audit.digest, MILLION_DIGEST, "got {:016x}", audit.digest);
    assert_eq!(audit.resting_orders, MILLION_RESTING_ORDERS);
    assert!(audit.conserved());
    assert_eq!(replay.stats.crossed_updates, 0);
    assert_eq!(replay.stats.exec_e_at_front, replay.stats.exec_e);
}

#[test]
fn first_million_messages_of_the_real_day() {
    let path = Path::new(REAL_DAY);
    if !path.exists() {
        eprintln!("skipped: {REAL_DAY} is not present (run scripts/fetch_data first)");
        return;
    }
    let (replay, audit) = replay(fs::File::open(path).unwrap(), 1_000_000);
    assert_eq!(replay.stats.messages, 1_000_000);
    assert_eq!(
        audit.digest, REAL_FIRST_MILLION_DIGEST,
        "got {:016x}",
        audit.digest
    );
    assert_eq!(audit.resting_orders, REAL_FIRST_MILLION_RESTING_ORDERS);
    assert!(audit.conserved());
}

#[test]
fn smoke_reproduce_is_deterministic_and_matches_the_committed_tables() {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let mut outputs = Vec::new();
    for run in ["smoke_a", "smoke_b"] {
        let out = tmp.join(run);
        let _ = fs::remove_dir_all(&out);
        let status = Command::new(env!("CARGO_BIN_EXE_agora"))
            .args(["reproduce", "--fast", "--out"])
            .arg(&out)
            .status()
            .unwrap();
        assert!(status.success(), "reproduce --fast failed");
        outputs.push(out);
    }
    for name in ["messages_by_type.csv", "invariants.csv"] {
        let a = fs::read(outputs[0].join(name)).unwrap();
        let b = fs::read(outputs[1].join(name)).unwrap();
        assert!(a == b, "{name} differs between two runs");
        let committed = fs::read(Path::new("results/smoke").join(name)).unwrap();
        assert!(a == committed, "{name} differs from results/smoke/{name}");
    }
}
