//! The `agora` command: replay an ITCH 5.0 file, generate a synthetic one,
//! and rebuild `results/`.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use agora::hist::{ticks, Calibration, Histogram};
use agora::itch::{parse, Body, FrameReader, TYPES};
use agora::pin::{busy_cpus, machine_cpu_times, pin_to_cpu};
use agora::replay::{Audit, RefIndex, ReplacePolicy, Replay, Stats};
use agora::report::{self, LatencyRow};
use agora::synth::{generate, SynthConfig};

/// The sample day the full reproduce run uses.
const DAY: &str = "12302019.NASDAQ_ITCH50";
/// The committed synthetic stream the smoke run uses.
const SMOKE_FILE: &str = "tests/data/synthetic.itch";
/// How the committed synthetic stream was generated.
const SMOKE: SynthConfig = SynthConfig {
    seed: 20_191_230,
    messages: 50_000,
    symbols: 8,
};

const USAGE: &str = "\
usage:
  agora replay <file> [--check] [--latency] [--limit N] [--replace-keeps-priority]
      Rebuild every book from an ITCH 5.0 file (.gz is read directly).
      --replace-keeps-priority measures an alternative reading of Order
      Replace; see PAPER.md.
  agora synth <out> [--messages N] [--symbols N] [--seed N]
      Write a synthetic ITCH 5.0 stream.
  agora gunzip <in.gz> <out>
      Decompress a sample file.
  agora reproduce [--fast] [--data FILE] [--out DIR] [--runs N] [--cpu N]
      Rebuild results/. --fast uses the committed synthetic stream and
      writes results/smoke/. The full run pins itself to logical CPU N
      (default 2) and times every pass --runs times (default 10).
";

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn open(path: &Path) -> io::Result<Box<dyn Read>> {
    let file = File::open(path)?;
    if path.extension().is_some_and(|e| e == "gz") {
        Ok(Box::new(flate2::read::MultiGzDecoder::new(file)))
    } else {
        Ok(Box::new(file))
    }
}

fn reader(path: &Path) -> io::Result<FrameReader<Box<dyn Read>>> {
    Ok(FrameReader::with_capacity(open(path)?, 4 << 20))
}

/// Times of day at which the full run fingerprints every book.
const CHECKPOINTS: [(&str, u64); 8] = [
    ("07:00:00", 7 * 3600),
    ("09:30:00", 9 * 3600 + 1800),
    ("09:35:00", 9 * 3600 + 2100),
    ("11:00:00", 11 * 3600),
    ("13:00:00", 13 * 3600),
    ("15:00:00", 15 * 3600),
    ("16:00:00", 16 * 3600),
    ("18:00:00", 18 * 3600),
];

/// The state of every book just before the first message at or after a
/// time of day.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Checkpoint {
    label: &'static str,
    messages: u64,
    resting_orders: u64,
    digest: u64,
}

/// What one pass over a file produced.
struct Pass {
    replay: Replay,
    seconds: f64,
    parse_errors: u64,
    first_parse_error: Option<String>,
    checkpoints: Vec<Checkpoint>,
}

impl Pass {
    fn rate(&self) -> f64 {
        self.replay.stats.messages as f64 / self.seconds
    }
}

/// Read, parse and apply every message. This is the timed path.
fn replay_pass(path: &Path, check: bool, limit: u64) -> Res<Pass> {
    replay_pass_with(path, check, limit, ReplacePolicy::BackOfQueue, false)
}

fn replay_pass_with(
    path: &Path,
    check: bool,
    limit: u64,
    policy: ReplacePolicy,
    mark: bool,
) -> Res<Pass> {
    let mut frames = reader(path)?;
    let mut replay = Replay::new(check).with_policy(policy);
    let mut parse_errors = 0;
    let mut first_parse_error = None;
    let mut checkpoints = Vec::new();
    // With checkpoints off this stays at the maximum and the test below is
    // one never-taken branch per message.
    let mut next_mark = if mark {
        CHECKPOINTS[0].1 * 1_000_000_000
    } else {
        u64::MAX
    };
    let start = Instant::now();
    let mut seen = 0;
    while seen < limit {
        let Some(frame) = frames.next_frame()? else {
            break;
        };
        seen += 1;
        match parse(frame) {
            Ok(m) => {
                while m.header.timestamp >= next_mark {
                    let (label, _) = CHECKPOINTS[checkpoints.len()];
                    checkpoints.push(Checkpoint {
                        label,
                        messages: replay.stats.messages,
                        resting_orders: replay.live_orders() as u64,
                        digest: replay.digest(),
                    });
                    next_mark = CHECKPOINTS
                        .get(checkpoints.len())
                        .map_or(u64::MAX, |c| c.1 * 1_000_000_000);
                }
                replay.apply(&m)
            }
            Err(e) => {
                parse_errors += 1;
                if first_parse_error.is_none() {
                    first_parse_error = Some(format!("message {seen}: {e}"));
                }
            }
        }
    }
    Ok(Pass {
        replay,
        seconds: start.elapsed().as_secs_f64(),
        parse_errors,
        first_parse_error,
        checkpoints,
    })
}

/// Read every frame without parsing it. Returns the message count and the
/// seconds taken.
fn frames_pass(path: &Path) -> Res<(u64, f64)> {
    let mut frames = reader(path)?;
    let start = Instant::now();
    let (mut n, mut sink) = (0u64, 0u64);
    while let Some(frame) = frames.next_frame()? {
        n += 1;
        sink = sink.wrapping_add(u64::from(std::hint::black_box(frame)[0]));
    }
    std::hint::black_box(sink);
    Ok((n, start.elapsed().as_secs_f64()))
}

/// Read and parse every message without touching a book. Returns the message
/// count and the seconds taken.
fn parse_pass(path: &Path) -> Res<(u64, f64)> {
    let mut frames = reader(path)?;
    let start = Instant::now();
    let (mut n, mut sink) = (0u64, 0u64);
    while let Some(frame) = frames.next_frame()? {
        n += 1;
        if let Ok(m) = parse(frame) {
            sink = sink.wrapping_add(std::hint::black_box(&m).header.timestamp);
        }
    }
    std::hint::black_box(sink);
    Ok((n, start.elapsed().as_secs_f64()))
}

/// Read, parse and do only the order-reference lookups a replay would do:
/// insert on add, look up on execute and cancel, remove on delete, both on
/// replace. No book is touched. An order that an execution or a cancel
/// empties stays in the index here, because only a book knows it is empty;
/// that is about 2% of the day's orders.
fn index_pass(path: &Path) -> Res<(u64, f64)> {
    let mut frames = reader(path)?;
    let mut refs = RefIndex::default();
    let start = Instant::now();
    let (mut n, mut sink) = (0u64, 0u64);
    while let Some(frame) = frames.next_frame()? {
        n += 1;
        let Ok(m) = parse(frame) else { continue };
        match m.body {
            Body::AddOrder { order_ref, .. } => {
                refs.insert(order_ref, n);
            }
            Body::OrderExecuted { order_ref, .. }
            | Body::OrderExecutedWithPrice { order_ref, .. }
            | Body::OrderCancel { order_ref, .. } => {
                sink = sink.wrapping_add(refs.get(&order_ref).copied().unwrap_or(0));
            }
            Body::OrderDelete { order_ref } => {
                sink = sink.wrapping_add(refs.remove(&order_ref).unwrap_or(0));
            }
            Body::OrderReplace {
                old_ref, new_ref, ..
            } => {
                sink = sink.wrapping_add(refs.remove(&old_ref).unwrap_or(0));
                refs.insert(new_ref, n);
            }
            _ => {}
        }
    }
    std::hint::black_box(sink);
    Ok((n, start.elapsed().as_secs_f64()))
}

/// Time each message from before parse to after apply, by message type.
/// Returns one histogram per type byte, in ticks, the nanoseconds per tick,
/// and the cost of an empty measurement in ticks.
fn latency_pass(path: &Path) -> Res<(Vec<Histogram>, f64, Histogram, f64)> {
    let mut frames = reader(path)?;
    let mut replay = Replay::new(false);
    let mut hists = vec![Histogram::new(); 256];
    let cal = Calibration::start();
    let start = Instant::now();
    while let Some(frame) = frames.next_frame()? {
        let t0 = ticks();
        if let Ok(m) = parse(frame) {
            replay.apply(&m);
        }
        let t1 = ticks();
        hists[usize::from(frame[0])].record(t1.wrapping_sub(t0));
    }
    let seconds = start.elapsed().as_secs_f64();
    let ns_per_tick = cal.ns_per_tick();
    let mut floor = Histogram::new();
    for _ in 0..1_000_000 {
        let t0 = ticks();
        let t1 = ticks();
        floor.record(t1.wrapping_sub(t0));
    }
    Ok((hists, ns_per_tick, floor, seconds))
}

fn latency_row(label: &str, h: &Histogram, ns_per_tick: f64) -> LatencyRow {
    let q = |p: f64| h.quantile(p) as f64 * ns_per_tick;
    LatencyRow {
        label: label.to_string(),
        count: h.count(),
        p50: q(0.5),
        p90: q(0.9),
        p99: q(0.99),
        p999: q(0.999),
        p9999: q(0.9999),
        max: h.max() as f64 * ns_per_tick,
    }
}

/// Rows for the latency table: every type seen, then the messages that change
/// a book, the ones that do not, and all of them.
fn latency_rows(hists: &[Histogram], ns_per_tick: f64, floor: &Histogram) -> Vec<LatencyRow> {
    let mut rows = Vec::new();
    let (mut book, mut other, mut all) = (Histogram::new(), Histogram::new(), Histogram::new());
    for (t, name, _, _) in TYPES {
        let h = &hists[usize::from(t)];
        all.merge(h);
        if b"AFECXDU".contains(&t) {
            book.merge(h);
        } else {
            other.merge(h);
        }
        if h.count() > 0 {
            rows.push(latency_row(
                &format!("{} ({})", name, t as char),
                h,
                ns_per_tick,
            ));
        }
    }
    rows.push(latency_row("Book-changing messages", &book, ns_per_tick));
    rows.push(latency_row("Other messages", &other, ns_per_tick));
    rows.push(latency_row("All messages", &all, ns_per_tick));
    rows.push(latency_row("Empty measurement", floor, ns_per_tick));
    rows
}

/// The rows the chart shows: the seven book-changing types and the rest.
fn chart_rows(hists: &[Histogram], ns_per_tick: f64) -> Vec<LatencyRow> {
    let labels = [
        (b'A', "Add order (A)"),
        (b'F', "Add order, attributed (F)"),
        (b'E', "Execute (E)"),
        (b'C', "Execute with price (C)"),
        (b'X', "Partial cancel (X)"),
        (b'D', "Delete (D)"),
        (b'U', "Replace (U)"),
    ];
    let mut rows: Vec<LatencyRow> = labels
        .iter()
        .filter(|(t, _)| hists[usize::from(*t)].count() > 0)
        .map(|(t, label)| latency_row(label, &hists[usize::from(*t)], ns_per_tick))
        .collect();
    let mut other = Histogram::new();
    for (t, _, _, _) in TYPES {
        if !b"AFECXDU".contains(&t) {
            other.merge(&hists[usize::from(t)]);
        }
    }
    if other.count() > 0 {
        rows.push(latency_row(
            "No book change (all other)",
            &other,
            ns_per_tick,
        ));
    }
    rows
}

fn print_summary(pass: &Pass, audit: Option<&Audit>) {
    let s = &pass.replay.stats;
    println!(
        "{} messages in {:.3} s ({:.2} million per second)",
        s.messages,
        pass.seconds,
        pass.rate() / 1e6
    );
    println!(
        "  {} to {}",
        report::clock(s.first_ts),
        report::clock(s.last_ts)
    );
    if let Some(e) = &pass.first_parse_error {
        println!("  {} parse errors; first: {e}", pass.parse_errors);
    }
    println!(
        "  orders added {}, peak resting {}, anomalies: unknown ref {}, duplicate ref {}, over-reduce {}",
        s.orders_added, s.peak_live_orders, s.unknown_ref, s.duplicate_ref, s.over_reduce
    );
    if let Some(a) = audit {
        println!(
            "  crossed updates {} of {} in {} episodes; volume conserved: {} (ledger gap {})",
            s.crossed_updates,
            s.book_updates,
            s.crossed_episodes,
            a.conserved(),
            a.ledger_gap
        );
        println!(
            "  E executions against the oldest order at the best price: {} of {}",
            s.exec_e_at_front, s.exec_e
        );
        println!("  book digest {:016x}", a.digest);
    }
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn value<T: std::str::FromStr>(args: &[String], name: &str) -> Res<Option<T>> {
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(i) => {
            let raw = args
                .get(i + 1)
                .ok_or_else(|| format!("{name} needs a value"))?;
            raw.parse()
                .map(Some)
                .map_err(|_| format!("bad value for {name}: {raw}").into())
        }
    }
}

fn write(path: &Path, text: &str) -> Res<()> {
    fs::write(path, text.as_bytes())?;
    println!("wrote {}", path.display());
    Ok(())
}

fn cmd_replay(args: &[String]) -> Res<()> {
    let path = PathBuf::from(args.first().ok_or("replay needs a file")?);
    let limit = value::<u64>(args, "--limit")?.unwrap_or(u64::MAX);
    if flag(args, "--latency") {
        let (hists, ns_per_tick, floor, _) = latency_pass(&path)?;
        print!(
            "{}",
            report::latency_csv(&latency_rows(&hists, ns_per_tick, &floor))
        );
        return Ok(());
    }
    let check = flag(args, "--check");
    let policy = if flag(args, "--replace-keeps-priority") {
        ReplacePolicy::KeepIfSamePriceAndNotLarger
    } else {
        ReplacePolicy::BackOfQueue
    };
    let mut pass = replay_pass_with(&path, check, limit, policy, false)?;
    let audit = check.then(|| pass.replay.finish());
    print_summary(&pass, audit.as_ref());
    Ok(())
}

fn cmd_synth(args: &[String]) -> Res<()> {
    let out = PathBuf::from(args.first().ok_or("synth needs an output file")?);
    let cfg = SynthConfig {
        seed: value(args, "--seed")?.unwrap_or(SMOKE.seed),
        messages: value(args, "--messages")?.unwrap_or(SMOKE.messages),
        symbols: value(args, "--symbols")?.unwrap_or(SMOKE.symbols),
    };
    let synth = generate(&cfg);
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&out, &synth.bytes)?;
    println!(
        "wrote {}: {} messages, {} bytes, book digest {:016x}",
        out.display(),
        synth.messages,
        synth.bytes.len(),
        synth.digest
    );
    Ok(())
}

fn gunzip(src: &Path, dst: &Path) -> Res<u64> {
    let mut input = flate2::read::MultiGzDecoder::new(File::open(src)?);
    let tmp = dst.with_extension("partial");
    let mut output = io::BufWriter::with_capacity(4 << 20, File::create(&tmp)?);
    let bytes = io::copy(&mut input, &mut output)?;
    output.flush()?;
    drop(output);
    fs::rename(&tmp, dst)?;
    Ok(bytes)
}

fn cmd_gunzip(args: &[String]) -> Res<()> {
    let (src, dst) = match args {
        [a, b, ..] => (PathBuf::from(a), PathBuf::from(b)),
        _ => return Err("gunzip needs an input and an output".into()),
    };
    let bytes = gunzip(&src, &dst)?;
    println!("wrote {}: {bytes} bytes", dst.display());
    Ok(())
}

/// Smallest, median and largest of a set of timings.
fn spread(v: &[f64]) -> (f64, f64, f64) {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let median = if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    };
    (v[0], median, v[n - 1])
}

/// The smoke run: replay the committed synthetic stream with every check on,
/// compare it with the generator, and fail on any broken invariant.
fn reproduce_fast(out: &Path) -> Res<()> {
    let path = Path::new(SMOKE_FILE);
    let synth = generate(&SMOKE);
    let committed = fs::read(path)?;
    if committed != synth.bytes {
        return Err(format!(
            "{SMOKE_FILE} differs from what the generator makes for {SMOKE:?}; \
             regenerate it with `agora synth {SMOKE_FILE}`"
        )
        .into());
    }
    let mut pass = replay_pass(path, true, u64::MAX)?;
    let audit = pass.replay.finish();
    print_summary(&pass, Some(&audit));
    let unchecked = replay_pass(path, false, u64::MAX)?;
    println!(
        "unchecked replay: {:.2} million messages per second (tiny file; not a benchmark)",
        unchecked.rate() / 1e6
    );
    let (hists, ns_per_tick, floor, _) = latency_pass(path)?;
    let rows = latency_rows(&hists, ns_per_tick, &floor);
    let all = &rows[rows.len() - 2];
    println!(
        "latency, all messages: p50 {} p99 {}",
        report::human_ns(all.p50),
        report::human_ns(all.p99)
    );

    fs::create_dir_all(out)?;
    let s = &pass.replay.stats;
    write(&out.join("messages_by_type.csv"), &report::messages_csv(s))?;
    let mut invariants = report::invariants_csv(s, &audit, pass.parse_errors);
    invariants.push_str(&format!("book_digest,{:016x}\n", audit.digest));
    write(&out.join("invariants.csv"), &invariants)?;

    let mut failures = Vec::new();
    if pass.parse_errors > 0 {
        failures.push(format!("{} parse errors", pass.parse_errors));
    }
    if s.messages != synth.messages || s.counts != synth.counts {
        failures.push("message counts differ from the generator".into());
    }
    if audit.digest != synth.digest || unchecked.replay.digest() != synth.digest {
        failures.push("rebuilt books differ from the generator's books".into());
    }
    if !audit.conserved() {
        failures.push(format!("volume not conserved: {audit:?}"));
    }
    if s.crossed_updates > 0 || s.locked_updates > 0 {
        failures.push("a book was crossed or locked".into());
    }
    if s.exec_e_at_front != s.exec_e || s.exec_c_at_front != s.exec_c {
        failures.push("an execution skipped the queue".into());
    }
    if s.unknown_ref + s.duplicate_ref + s.over_reduce + s.locate_mismatch + s.zero_share_add > 0 {
        failures.push("the stream has order anomalies".into());
    }
    if failures.is_empty() {
        println!("smoke run passed");
        Ok(())
    } else {
        Err(failures.join("; ").into())
    }
}

/// The passes timed in every round of the full run, in the order they run.
const PASSES: [&str; 6] = [
    "read_frames_only",
    "read_and_parse",
    "read_parse_and_order_lookup",
    "replay",
    "replay_with_checks",
    "replay_with_per_message_timing",
];

/// The numbers every replay of one file must agree on.
fn ledger(s: &Stats) -> [u64; 8] {
    [
        s.messages,
        s.orders_added,
        s.shares_added,
        s.shares_executed,
        s.shares_cancelled,
        s.shares_deleted,
        s.shares_replaced,
        s.peak_live_orders,
    ]
}

/// The full run on one real day.
fn reproduce_full(data: &Path, out: &Path, runs: usize, cpu: usize) -> Res<()> {
    if !data.exists() {
        let gz = PathBuf::from(format!("{}.gz", data.display()));
        if !gz.exists() {
            return Err(format!(
                "{} not found. Run scripts/fetch_data.sh (or .ps1) first.",
                gz.display()
            )
            .into());
        }
        println!("decompressing {} ...", gz.display());
        let bytes = gunzip(&gz, data)?;
        println!("wrote {}: {bytes} bytes", data.display());
    }
    let file_bytes = fs::metadata(data)?.len();
    fs::create_dir_all(out)?;
    let cpus = std::thread::available_parallelism().map_or(1, usize::from);
    let pinned = pin_to_cpu(cpu);
    println!("pinned to cpu {cpu}: {pinned}");

    // Warm-up, not timed: two parse passes pull the file into the page
    // cache, then one full replay. The warm-up replay also takes the
    // intraday checkpoints.
    println!("warm-up ...");
    parse_pass(data)?;
    parse_pass(data)?;
    let warm = replay_pass_with(data, false, u64::MAX, ReplacePolicy::BackOfQueue, true)?;

    // The pass every count and invariant comes from. Not timed.
    println!("checked pass ...");
    let mut checked = replay_pass_with(data, true, u64::MAX, ReplacePolicy::BackOfQueue, true)?;
    let audit = checked.replay.finish();
    print_summary(&checked, Some(&audit));
    if checked.checkpoints != warm.checkpoints {
        return Err("checked and unchecked replays differ at a checkpoint".into());
    }
    let expected = ledger(&checked.replay.stats);
    let n = checked.replay.stats.messages;

    // Timed rounds. Each round runs every pass once, so slow drift on the
    // machine lands on all of them alike.
    let load_before = machine_cpu_times();
    let mut times: Vec<Vec<f64>> = vec![Vec::new(); PASSES.len()];
    let mut latency = None;
    for round in 1..=runs {
        println!("round {round} of {runs} ...");
        times[0].push(frames_pass(data)?.1);
        times[1].push(parse_pass(data)?.1);
        times[2].push(index_pass(data)?.1);
        for (slot, check) in [(3, false), (4, true)] {
            let pass = replay_pass(data, check, u64::MAX)?;
            if ledger(&pass.replay.stats) != expected {
                return Err("a timed replay disagrees with the checked pass".into());
            }
            times[slot].push(pass.seconds);
        }
        let (hists, ns_per_tick, floor, seconds) = latency_pass(data)?;
        times[5].push(seconds);
        latency = Some((hists, ns_per_tick, floor));
        println!(
            "  replay {:.3} s, with checks {:.3} s, with timing {:.3} s",
            times[3][round - 1],
            times[4][round - 1],
            times[5][round - 1]
        );
    }
    let (hists, ns_per_tick, floor) = latency.ok_or("no rounds were run")?;
    // What the whole machine did during the timed rounds, less this thread.
    let other_load = match (load_before, machine_cpu_times()) {
        (Some(a), Some(b)) => Some((busy_cpus(a, b, cpus) - 1.0).max(0.0)),
        _ => None,
    };

    println!("first-million pass ...");
    let mut first = replay_pass(data, true, 1_000_000)?;
    let first_audit = first.replay.finish();

    let mut throughput = String::from(
        "pass,runs,min_seconds,median_seconds,max_seconds,spread_percent,median_ns_per_message,median_million_messages_per_second\n",
    );
    let mut each = String::from("round,pass,seconds\n");
    let mut medians = Vec::new();
    for (name, t) in PASSES.iter().zip(&times) {
        let (lo, mid, hi) = spread(t);
        medians.push(mid);
        throughput.push_str(&format!(
            "{name},{},{lo:.3},{mid:.3},{hi:.3},{:.1},{:.1},{:.3}\n",
            t.len(),
            (hi - lo) / mid * 100.0,
            mid * 1e9 / n as f64,
            n as f64 / mid / 1e6
        ));
        for (i, secs) in t.iter().enumerate() {
            each.push_str(&format!("{},{name},{secs:.3}\n", i + 1));
        }
    }
    let per_message = |secs: f64| secs * 1e9 / n as f64;
    let mut profile = String::from("stage,median_seconds,ns_per_message,share_of_replay\n");
    let stages = [
        ("read the file and split it into messages", medians[0]),
        ("parse", medians[1] - medians[0]),
        ("order reference lookup", medians[2] - medians[1]),
        ("book update and counters", medians[3] - medians[2]),
        ("whole replay", medians[3]),
    ];
    for (name, secs) in stages {
        profile.push_str(&format!(
            "{name},{secs:.3},{:.1},{:.3}\n",
            per_message(secs),
            secs / medians[3]
        ));
    }

    let rows = latency_rows(&hists, ns_per_tick, &floor);
    let mut all = Histogram::new();
    for h in &hists {
        all.merge(h);
    }
    let mut timer = String::from("measure,value\n");
    let mut row = |k: &str, v: String| timer.push_str(&format!("{k},{v}\n"));
    row(
        "pinned_cpu",
        if pinned {
            cpu.to_string()
        } else {
            "not pinned".into()
        },
    );
    row("rounds", runs.to_string());
    row("logical_cpus", cpus.to_string());
    row(
        "other_load_in_cpus_during_timed_rounds",
        other_load.map_or("not measured".into(), |l| format!("{l:.2}")),
    );
    row(
        "replay_median_ns_per_message",
        format!("{:.1}", per_message(medians[3])),
    );
    row(
        "replay_with_per_message_timing_median_ns_per_message",
        format!("{:.1}", per_message(medians[5])),
    );
    row(
        "timing_cost_ns_per_message",
        format!("{:.1}", per_message(medians[5] - medians[3])),
    );
    row(
        "mean_measured_latency_ns",
        format!("{:.1}", all.mean() * ns_per_tick),
    );
    row(
        "two_clock_reads_back_to_back_median_ns",
        format!("{:.1}", floor.quantile(0.5) as f64 * ns_per_tick),
    );
    row("ns_per_tick", format!("{ns_per_tick:.6}"));

    let mut marks = String::from("time_of_day,messages_before,resting_orders,book_digest\n");
    for c in &checked.checkpoints {
        marks.push_str(&format!(
            "{},{},{},{:016x}\n",
            c.label, c.messages, c.resting_orders, c.digest
        ));
    }
    marks.push_str(&format!(
        "after_first_million_messages,1000000,{},{:016x}\n",
        first_audit.resting_orders, first_audit.digest
    ));

    let s = &checked.replay.stats;
    write(&out.join("messages_by_type.csv"), &report::messages_csv(s))?;
    let mut invariants = report::invariants_csv(s, &audit, checked.parse_errors);
    invariants.push_str(&format!("file_bytes,{file_bytes}\n"));
    write(&out.join("invariants.csv"), &invariants)?;
    write(&out.join("checkpoints.csv"), &marks)?;
    write(
        &out.join("crossed_symbols.csv"),
        &report::crossed_symbols_csv(&checked.replay, 50),
    )?;
    write(
        &out.join("crossed_examples.csv"),
        &report::crossed_examples_csv(&checked.replay),
    )?;
    write(&out.join("throughput.csv"), &throughput)?;
    write(&out.join("throughput_runs.csv"), &each)?;
    write(&out.join("profile.csv"), &profile)?;
    write(&out.join("timer_overhead.csv"), &timer)?;
    write(
        &out.join("latency_percentiles.csv"),
        &report::latency_csv(&rows),
    )?;
    let name = data
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let title = "Time to parse and apply one ITCH message, by type";
    let subtitle = format!(
        "{name}: {} messages, one pinned core. Median replay of the day: {:.1} s.",
        s.messages, medians[3]
    );
    write(
        &out.join("latency.svg"),
        &report::latency_svg(&chart_rows(&hists, ns_per_tick), title, &subtitle),
    )?;
    println!(
        "headline: {} messages, every book rebuilt in {:.2} s (median of {runs})",
        s.messages, medians[3]
    );
    Ok(())
}

fn cmd_reproduce(args: &[String]) -> Res<()> {
    let fast = flag(args, "--fast");
    let out = value::<PathBuf>(args, "--out")?
        .unwrap_or_else(|| PathBuf::from(if fast { "results/smoke" } else { "results" }));
    if fast {
        return reproduce_fast(&out);
    }
    let data = value::<PathBuf>(args, "--data")?.unwrap_or_else(|| Path::new("data").join(DAY));
    let runs = value::<usize>(args, "--runs")?.unwrap_or(10).max(1);
    let cpu = value::<usize>(args, "--cpu")?.unwrap_or(2);
    reproduce_full(&data, &out, runs, cpu)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("replay") => cmd_replay(&args[1..]),
        Some("synth") => cmd_synth(&args[1..]),
        Some("gunzip") => cmd_gunzip(&args[1..]),
        Some("reproduce") => cmd_reproduce(&args[1..]),
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
