//! The `agora` command: replay an ITCH 5.0 file, generate a synthetic one,
//! and rebuild `results/`.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use agora::hist::{ticks, Calibration, Histogram};
use agora::index::{HashIndex, OpenIndex, OrderIndex, PageIndex};
use agora::itch::{parse, Body, FrameReader, TYPES};
use agora::pin::{busy_cpus, machine_cpu_times, pin_to_cpu};
use agora::priority::{Priority, PriorityReport};
use agora::replay::{Audit, ReplacePolicy, Replay, Resting, Stats};
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
  agora replay <file> [--check] [--latency] [--limit N] [--batched]
               [--batch N] [--replace-keeps-priority]
      Rebuild every book from an ITCH 5.0 file (.gz is read directly).
      --batched parses 32 messages ahead (or --batch N) and prefetches the
      orders they touch; it ignores --limit and --replace-keeps-priority.
      --replace-keeps-priority measures an alternative reading of Order
      Replace; see PAPER.md.
  agora priority <file> [--out DIR] [--replace-keeps-priority]
      Look at every execution that skipped the queue and test what could
      explain it. Prints the table; with --out also writes priority.csv and
      priority_by_time.csv there.
  agora synth <out> [--messages N] [--symbols N] [--seed N]
      Write a synthetic ITCH 5.0 stream.
  agora gunzip <in.gz> <out>
      Decompress a sample file.
  agora bench <file> --out DIR [--runs N] [--cpu N]
      Only the timings: pin to logical CPU N (default 2), warm up, then
      time every pass once in each of --runs rounds (default 10).
  agora reproduce [--fast] [--data FILE] [--out DIR] [--runs N] [--cpu N]
      Rebuild results/. --fast uses the committed synthetic stream and
      writes results/smoke/. The full run writes the counts, invariants,
      checkpoints and queue-priority tables, then runs the bench.
      --runs 0 skips the timings.
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

/// Take checkpoint number `i` of a replay.
fn checkpoint(replay: &Replay, i: usize) -> Checkpoint {
    Checkpoint {
        label: CHECKPOINTS[i].0,
        messages: replay.stats.messages,
        resting_orders: replay.live_orders() as u64,
        digest: replay.digest(),
    }
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
                    checkpoints.push(checkpoint(&replay, checkpoints.len()));
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

/// Messages parsed ahead of the one being applied in the batched replay,
/// unless `--batch` says otherwise.
const BATCH: usize = 32;

/// Read, parse and apply every message, a batch at a time. Each batch is
/// parsed first, then the replay is told which orders the batch will touch so
/// the CPU can start fetching them, then the batch is applied in order. The
/// books come out the same as from [`replay_pass`]; only the timing differs.
fn replay_pass_batched(path: &Path, check: bool, mark: bool, size: usize) -> Res<Pass> {
    let mut frames = reader(path)?;
    let mut replay = Replay::new(check);
    let mut checkpoints = Vec::new();
    let mut next_mark = if mark {
        CHECKPOINTS[0].1 * 1_000_000_000
    } else {
        u64::MAX
    };
    let mut parse_errors = 0;
    let mut first_parse_error = None;
    let size = size.max(1);
    let mut batch = Vec::with_capacity(size);
    let mut seen = 0u64;
    let start = Instant::now();
    loop {
        batch.clear();
        while batch.len() < size {
            let Some(frame) = frames.next_frame()? else {
                break;
            };
            seen += 1;
            match parse(frame) {
                Ok(m) => batch.push(m),
                Err(e) => {
                    parse_errors += 1;
                    if first_parse_error.is_none() {
                        first_parse_error = Some(format!("message {seen}: {e}"));
                    }
                }
            }
        }
        if batch.is_empty() {
            break;
        }
        for m in &batch {
            replay.prefetch_index(m);
        }
        for m in &batch {
            replay.prefetch_order(m);
        }
        for m in &batch {
            replay.prefetch_links(m);
        }
        for m in &batch {
            while m.header.timestamp >= next_mark {
                checkpoints.push(checkpoint(&replay, checkpoints.len()));
                next_mark = CHECKPOINTS
                    .get(checkpoints.len())
                    .map_or(u64::MAX, |c| c.1 * 1_000_000_000);
            }
            replay.apply(m);
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
fn index_pass<I: OrderIndex<u64>>(path: &Path) -> Res<f64> {
    let mut frames = reader(path)?;
    let mut refs = I::default();
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
                sink = sink.wrapping_add(refs.get(order_ref).unwrap_or(0));
            }
            Body::OrderDelete { order_ref } => {
                sink = sink.wrapping_add(refs.remove(order_ref).unwrap_or(0));
            }
            Body::OrderReplace {
                old_ref, new_ref, ..
            } => {
                sink = sink.wrapping_add(refs.remove(old_ref).unwrap_or(0));
                refs.insert(new_ref, n);
            }
            _ => {}
        }
    }
    std::hint::black_box(sink);
    Ok(start.elapsed().as_secs_f64())
}

/// Replay on index `I` and note how much memory the index holds at each
/// checkpoint. Not timed.
fn index_memory_pass<I: OrderIndex<Resting>>(path: &Path) -> Res<Vec<usize>> {
    let mut frames = reader(path)?;
    let mut replay: Replay<I> = Replay::with_index(false);
    let mut sizes = Vec::new();
    while let Some(frame) = frames.next_frame()? {
        let Ok(m) = parse(frame) else { continue };
        while sizes.len() < CHECKPOINTS.len()
            && m.header.timestamp >= CHECKPOINTS[sizes.len()].1 * 1_000_000_000
        {
            sizes.push(replay.index().bytes());
        }
        replay.apply(&m);
    }
    Ok(sizes)
}

/// The timed replay, one message at a time, on either index. Returns the
/// seconds taken and the counters every replay must agree on.
fn timed_replay<I: OrderIndex<Resting>>(path: &Path, check: bool) -> Res<(f64, [u64; 8])> {
    let mut frames = reader(path)?;
    let mut replay: Replay<I> = Replay::with_index(check);
    let start = Instant::now();
    while let Some(frame) = frames.next_frame()? {
        if let Ok(m) = parse(frame) {
            replay.apply(&m);
        }
    }
    let seconds = start.elapsed().as_secs_f64();
    Ok((seconds, ledger(&replay.stats)))
}

/// The timed replay in batches of [`BATCH`] with prefetch hints, on either
/// index.
fn timed_replay_batched<I: OrderIndex<Resting>>(path: &Path) -> Res<(f64, [u64; 8])> {
    let mut frames = reader(path)?;
    let mut replay: Replay<I> = Replay::with_index(false);
    let mut batch = Vec::with_capacity(BATCH);
    let start = Instant::now();
    loop {
        batch.clear();
        while batch.len() < BATCH {
            let Some(frame) = frames.next_frame()? else {
                break;
            };
            if let Ok(m) = parse(frame) {
                batch.push(m);
            }
        }
        if batch.is_empty() {
            break;
        }
        for m in &batch {
            replay.prefetch_index(m);
        }
        for m in &batch {
            replay.prefetch_order(m);
        }
        for m in &batch {
            replay.prefetch_links(m);
        }
        for m in &batch {
            replay.apply(m);
        }
    }
    let seconds = start.elapsed().as_secs_f64();
    Ok((seconds, ledger(&replay.stats)))
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
    let mut pass = if flag(args, "--batched") {
        replay_pass_batched(
            &path,
            check,
            false,
            value::<usize>(args, "--batch")?.unwrap_or(BATCH),
        )?
    } else {
        replay_pass_with(&path, check, limit, policy, false)?
    };
    let audit = check.then(|| pass.replay.finish());
    print_summary(&pass, audit.as_ref());
    Ok(())
}

/// Run the queue-priority analysis over a file.
fn priority_pass(path: &Path, policy: ReplacePolicy) -> Res<PriorityReport> {
    let mut frames = reader(path)?;
    let mut priority = Priority::with_policy(policy);
    while let Some(frame) = frames.next_frame()? {
        if let Ok(m) = parse(frame) {
            priority.apply(&m);
        }
    }
    Ok(priority.report())
}

fn cmd_priority(args: &[String]) -> Res<()> {
    let path = PathBuf::from(args.first().ok_or("priority needs a file")?);
    let policy = if flag(args, "--replace-keeps-priority") {
        ReplacePolicy::KeepIfSamePriceAndNotLarger
    } else {
        ReplacePolicy::BackOfQueue
    };
    let report = priority_pass(&path, policy)?;
    print!("{}", report.to_csv());
    if let Some(out) = value::<PathBuf>(args, "--out")? {
        fs::create_dir_all(&out)?;
        write(&out.join("priority.csv"), &report.to_csv())?;
        write(&out.join("priority_by_time.csv"), &report.by_time_csv())?;
    }
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
    if s.unknown_ref
        + s.duplicate_ref
        + s.over_reduce
        + s.locate_mismatch
        + s.zero_share_add
        + s.book_faults
        > 0
    {
        failures.push("the stream has order anomalies".into());
    }
    if failures.is_empty() {
        println!("smoke run passed");
        Ok(())
    } else {
        Err(failures.join("; ").into())
    }
}

/// The passes timed in every round, in the order they run.
const PASSES: [&str; 12] = [
    "read_frames_only",
    "read_and_parse",
    "lookup_only_std_hash_map",
    "lookup_only_page_table",
    "lookup_only",
    "replay_std_hash_map",
    "replay_page_table",
    "replay",
    "replay_batched_std_hash_map",
    "replay_batched",
    "replay_with_checks",
    "replay_with_per_message_timing",
];
// Positions in `PASSES`.
const FRAMES: usize = 0;
const PARSE: usize = 1;
const LOOKUP_STD: usize = 2;
const LOOKUP_PAGE: usize = 3;
const LOOKUP: usize = 4;
const REPLAY_STD: usize = 5;
const REPLAY_PAGE: usize = 6;
const REPLAY: usize = 7;
const BATCHED_STD: usize = 8;
const BATCHED: usize = 9;
const CHECKED: usize = 10;
const TIMED: usize = 11;

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

/// What the timed rounds measured.
struct Bench {
    runs: usize,
    cpu: usize,
    cpus: usize,
    pinned: bool,
    /// Seconds per pass per round, in [`PASSES`] order.
    times: Vec<Vec<f64>>,
    /// Load from everything else on the machine during the rounds, in CPUs.
    other_load: Option<f64>,
    hists: Vec<Histogram>,
    ns_per_tick: f64,
    floor: Histogram,
    messages: u64,
}

/// Pin to one CPU, warm up, then time every pass once per round. Each round
/// runs every pass, so slow drift on the machine lands on all of them alike.
fn bench(data: &Path, runs: usize, cpu: usize, expected: Option<[u64; 8]>) -> Res<Bench> {
    let cpus = std::thread::available_parallelism().map_or(1, usize::from);
    let pinned = pin_to_cpu(cpu);
    println!("pinned to cpu {cpu}: {pinned}");
    // Warm-up, not timed: a parse pass pulls the file into the page cache,
    // then one full replay.
    println!("warm-up ...");
    parse_pass(data)?;
    let (_, warm) = timed_replay::<OpenIndex<Resting>>(data, false)?;
    let expected = expected.unwrap_or(warm);
    if warm != expected {
        return Err("the warm-up replay disagrees with the checked pass".into());
    }
    let agree = |pass: usize, got: [u64; 8]| -> Res<()> {
        if got == expected {
            Ok(())
        } else {
            Err(format!(
                "{} counted differently from the other replays",
                PASSES[pass]
            )
            .into())
        }
    };

    let load_before = machine_cpu_times();
    let mut times: Vec<Vec<f64>> = vec![Vec::new(); PASSES.len()];
    let mut latency = None;
    for round in 1..=runs {
        println!("round {round} of {runs} ...");
        times[FRAMES].push(frames_pass(data)?.1);
        times[PARSE].push(parse_pass(data)?.1);
        times[LOOKUP_STD].push(index_pass::<HashIndex<u64>>(data)?);
        times[LOOKUP_PAGE].push(index_pass::<PageIndex<u64>>(data)?);
        times[LOOKUP].push(index_pass::<OpenIndex<u64>>(data)?);
        let (secs, got) = timed_replay::<HashIndex<Resting>>(data, false)?;
        agree(REPLAY_STD, got)?;
        times[REPLAY_STD].push(secs);
        let (secs, got) = timed_replay::<PageIndex<Resting>>(data, false)?;
        agree(REPLAY_PAGE, got)?;
        times[REPLAY_PAGE].push(secs);
        let (secs, got) = timed_replay::<OpenIndex<Resting>>(data, false)?;
        agree(REPLAY, got)?;
        times[REPLAY].push(secs);
        let (secs, got) = timed_replay_batched::<HashIndex<Resting>>(data)?;
        agree(BATCHED_STD, got)?;
        times[BATCHED_STD].push(secs);
        let (secs, got) = timed_replay_batched::<OpenIndex<Resting>>(data)?;
        agree(BATCHED, got)?;
        times[BATCHED].push(secs);
        let (secs, got) = timed_replay::<OpenIndex<Resting>>(data, true)?;
        agree(CHECKED, got)?;
        times[CHECKED].push(secs);
        let (hists, ns_per_tick, floor, seconds) = latency_pass(data)?;
        times[TIMED].push(seconds);
        latency = Some((hists, ns_per_tick, floor));
        println!(
            "  first version {:.3} s, page table {:.3} s, now {:.3} s, batched {:.3} s",
            times[REPLAY_STD][round - 1],
            times[REPLAY_PAGE][round - 1],
            times[REPLAY][round - 1],
            times[BATCHED][round - 1]
        );
    }
    let (hists, ns_per_tick, floor) = latency.ok_or("no rounds were run")?;
    // What the whole machine did during the timed rounds, less this thread.
    let other_load = match (load_before, machine_cpu_times()) {
        (Some(a), Some(b)) => Some((busy_cpus(a, b, cpus) - 1.0).max(0.0)),
        _ => None,
    };
    Ok(Bench {
        runs,
        cpu,
        cpus,
        pinned,
        times,
        other_load,
        hists,
        ns_per_tick,
        floor,
        messages: expected[0],
    })
}

/// Write the timing tables and the chart.
fn write_bench(out: &Path, b: &Bench, name: &str) -> Res<()> {
    let n = b.messages;
    let per_message = |secs: f64| secs * 1e9 / n as f64;
    let mut throughput = String::from(
        "pass,runs,min_seconds,median_seconds,max_seconds,spread_percent,median_ns_per_message,median_million_messages_per_second\n",
    );
    let mut each = String::from("round,pass,seconds\n");
    let mut medians = Vec::new();
    for (name, t) in PASSES.iter().zip(&b.times) {
        let (lo, mid, hi) = spread(t);
        medians.push(mid);
        throughput.push_str(&format!(
            "{name},{},{lo:.3},{mid:.3},{hi:.3},{:.1},{:.1},{:.3}\n",
            t.len(),
            (hi - lo) / mid * 100.0,
            per_message(mid),
            n as f64 / mid / 1e6
        ));
        for (i, secs) in t.iter().enumerate() {
            each.push_str(&format!("{},{name},{secs:.3}\n", i + 1));
        }
    }

    // Where the time goes, one message at a time, from the differences
    // between passes, for each of the three order indexes.
    let mut profile = String::from(
        "stage,std_hash_map_ns_per_message,page_table_ns_per_message,open_table_ns_per_message\n",
    );
    let read = medians[FRAMES];
    let parse = medians[PARSE] - medians[FRAMES];
    let stages = [
        ("read the file and split it into messages", [read; 3]),
        ("parse", [parse; 3]),
        (
            "order reference lookup",
            [
                medians[LOOKUP_STD] - medians[PARSE],
                medians[LOOKUP_PAGE] - medians[PARSE],
                medians[LOOKUP] - medians[PARSE],
            ],
        ),
        (
            "book update and counters",
            [
                medians[REPLAY_STD] - medians[LOOKUP_STD],
                medians[REPLAY_PAGE] - medians[LOOKUP_PAGE],
                medians[REPLAY] - medians[LOOKUP],
            ],
        ),
        (
            "whole replay",
            [medians[REPLAY_STD], medians[REPLAY_PAGE], medians[REPLAY]],
        ),
    ];
    for (stage, secs) in stages {
        profile.push_str(&format!(
            "{stage},{:.1},{:.1},{:.1}\n",
            per_message(secs[0]),
            per_message(secs[1]),
            per_message(secs[2])
        ));
    }

    // The versions tried, from the same rounds.
    let mut versions = String::from(
        "order_index,messages_applied,min_seconds,median_seconds,max_seconds,median_ns_per_message,median_million_messages_per_second,speedup_over_first_version\n",
    );
    for (index, how, slot) in [
        ("std hash map (first version)", "one at a time", REPLAY_STD),
        ("page table (tried)", "one at a time", REPLAY_PAGE),
        ("open table", "one at a time", REPLAY),
        (
            "std hash map (first version)",
            "32 at a time with prefetch",
            BATCHED_STD,
        ),
        ("open table", "32 at a time with prefetch", BATCHED),
    ] {
        let (lo, mid, hi) = spread(&b.times[slot]);
        versions.push_str(&format!(
            "{index},{how},{lo:.3},{mid:.3},{hi:.3},{:.1},{:.3},{:.2}\n",
            per_message(mid),
            n as f64 / mid / 1e6,
            medians[REPLAY_STD] / mid
        ));
    }

    let mut all = Histogram::new();
    for h in &b.hists {
        all.merge(h);
    }
    let mut timer = String::from("measure,value\n");
    let mut row = |k: &str, v: String| timer.push_str(&format!("{k},{v}\n"));
    row(
        "pinned_cpu",
        if b.pinned {
            b.cpu.to_string()
        } else {
            "not pinned".into()
        },
    );
    row("rounds", b.runs.to_string());
    row("logical_cpus", b.cpus.to_string());
    row(
        "other_load_in_cpus_during_timed_rounds",
        b.other_load
            .map_or("not measured".into(), |l| format!("{l:.2}")),
    );
    row(
        "replay_median_ns_per_message",
        format!("{:.1}", per_message(medians[REPLAY])),
    );
    row(
        "replay_with_per_message_timing_median_ns_per_message",
        format!("{:.1}", per_message(medians[TIMED])),
    );
    row(
        "timing_cost_ns_per_message",
        format!("{:.1}", per_message(medians[TIMED] - medians[REPLAY])),
    );
    row(
        "mean_measured_latency_ns",
        format!("{:.1}", all.mean() * b.ns_per_tick),
    );
    row(
        "two_clock_reads_back_to_back_median_ns",
        format!("{:.1}", b.floor.quantile(0.5) as f64 * b.ns_per_tick),
    );
    row("ns_per_tick", format!("{:.6}", b.ns_per_tick));

    fs::create_dir_all(out)?;
    write(&out.join("throughput.csv"), &throughput)?;
    write(&out.join("throughput_runs.csv"), &each)?;
    write(&out.join("profile.csv"), &profile)?;
    write(&out.join("versions.csv"), &versions)?;
    write(&out.join("timer_overhead.csv"), &timer)?;
    write(
        &out.join("latency_percentiles.csv"),
        &report::latency_csv(&latency_rows(&b.hists, b.ns_per_tick, &b.floor)),
    )?;
    let title = "Time to parse and apply one ITCH message, by type";
    let subtitle = format!(
        "{name}: {n} messages, one pinned core, one message at a time. Median replay of the day: {:.1} s.",
        medians[REPLAY]
    );
    write(
        &out.join("latency.svg"),
        &report::latency_svg(&chart_rows(&b.hists, b.ns_per_tick), title, &subtitle),
    )?;
    println!(
        "headline: {n} messages, every book rebuilt in {:.2} s one at a time, {:.2} s batched (medians of {})",
        medians[REPLAY], medians[BATCHED], b.runs
    );
    Ok(())
}

fn data_name(data: &Path) -> String {
    data.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Only the timed rounds, for any ITCH file.
fn cmd_bench(args: &[String]) -> Res<()> {
    let data = PathBuf::from(args.first().ok_or("bench needs a file")?);
    let out = value::<PathBuf>(args, "--out")?.ok_or("bench needs --out DIR")?;
    let runs = value::<usize>(args, "--runs")?.unwrap_or(10).max(1);
    let cpu = value::<usize>(args, "--cpu")?.unwrap_or(2);
    let b = bench(&data, runs, cpu, None)?;
    write_bench(&out, &b, &data_name(&data))
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

    // The pass every count and invariant comes from, with a book digest at
    // each checkpoint.
    println!("checked pass ...");
    let mut checked = replay_pass_with(data, true, u64::MAX, ReplacePolicy::BackOfQueue, true)?;
    let audit = checked.replay.finish();
    print_summary(&checked, Some(&audit));

    // The unchecked replay and the batched replay must build the same books
    // at every checkpoint.
    println!("unchecked and batched passes ...");
    let plain = replay_pass_with(data, false, u64::MAX, ReplacePolicy::BackOfQueue, true)?;
    let batched = replay_pass_batched(data, false, true, BATCH)?;
    if plain.checkpoints != checked.checkpoints || batched.checkpoints != checked.checkpoints {
        return Err("two replays differ at a checkpoint".into());
    }
    let expected = ledger(&checked.replay.stats);
    if ledger(&plain.replay.stats) != expected || ledger(&batched.replay.stats) != expected {
        return Err("two replays count differently".into());
    }

    println!("index memory passes ...");
    let memory = [
        index_memory_pass::<HashIndex<Resting>>(data)?,
        index_memory_pass::<PageIndex<Resting>>(data)?,
        index_memory_pass::<OpenIndex<Resting>>(data)?,
    ];
    let mut memory_csv = String::from(
        "time_of_day,resting_orders,std_hash_map_megabytes_estimate,page_table_megabytes,open_table_megabytes\n",
    );
    // The standard map reports less capacity as deleted slots pile up, though
    // it never gives memory back, so its column is the largest figure so far.
    let mut std_peak = 0.0f64;
    for (i, c) in checked.checkpoints.iter().enumerate() {
        let mb = |k: usize| memory[k].get(i).map_or(0.0, |b| *b as f64 / 1e6);
        std_peak = std_peak.max(mb(0));
        memory_csv.push_str(&format!(
            "{},{},{:.1},{:.1},{:.1}\n",
            c.label,
            c.resting_orders,
            std_peak,
            mb(1),
            mb(2)
        ));
    }

    println!("queue-priority passes ...");
    let priority = priority_pass(data, ReplacePolicy::BackOfQueue)?;
    let other_reading = priority_pass(data, ReplacePolicy::KeepIfSamePriceAndNotLarger)?;
    let s = &checked.replay.stats;
    if priority.executions != s.exec_e || priority.out_of_turn != s.exec_e - s.exec_e_at_front {
        return Err("the priority analysis and the checked replay count differently".into());
    }

    println!("first-million pass ...");
    let mut first = replay_pass(data, true, 1_000_000)?;
    let first_audit = first.replay.finish();

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

    write(&out.join("messages_by_type.csv"), &report::messages_csv(s))?;
    let mut invariants = report::invariants_csv(s, &audit, checked.parse_errors);
    invariants.push_str(&format!("file_bytes,{file_bytes}\n"));
    write(&out.join("invariants.csv"), &invariants)?;
    write(&out.join("checkpoints.csv"), &marks)?;
    write(&out.join("index_memory.csv"), &memory_csv)?;
    let mut priority_csv = priority.to_csv();
    priority_csv.push_str(&format!(
        "other_replace_reading_out_of_turn,{}\n",
        other_reading.out_of_turn
    ));
    priority_csv.push_str(&format!(
        "other_replace_reading_out_of_turn_not_a_burst_order,{}\n",
        other_reading.exec_other_out_of_turn
    ));
    write(&out.join("priority.csv"), &priority_csv)?;
    write(&out.join("priority_by_time.csv"), &priority.by_time_csv())?;
    write(
        &out.join("crossed_symbols.csv"),
        &report::crossed_symbols_csv(&checked.replay, 50),
    )?;
    write(
        &out.join("crossed_examples.csv"),
        &report::crossed_examples_csv(&checked.replay),
    )?;
    if runs == 0 {
        println!("timings skipped (--runs 0)");
        return Ok(());
    }
    drop((checked, plain, batched));
    let b = bench(data, runs, cpu, Some(expected))?;
    write_bench(out, &b, &data_name(data))
}

fn cmd_reproduce(args: &[String]) -> Res<()> {
    let fast = flag(args, "--fast");
    let out = value::<PathBuf>(args, "--out")?
        .unwrap_or_else(|| PathBuf::from(if fast { "results/smoke" } else { "results" }));
    if fast {
        return reproduce_fast(&out);
    }
    let data = value::<PathBuf>(args, "--data")?.unwrap_or_else(|| Path::new("data").join(DAY));
    let runs = value::<usize>(args, "--runs")?.unwrap_or(10);
    let cpu = value::<usize>(args, "--cpu")?.unwrap_or(2);
    reproduce_full(&data, &out, runs, cpu)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("replay") => cmd_replay(&args[1..]),
        Some("priority") => cmd_priority(&args[1..]),
        Some("bench") => cmd_bench(&args[1..]),
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
