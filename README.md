# Agora

A limit order book in Rust that rebuilds a full Nasdaq trading day from the exchange's own messages. The agora was the Greek marketplace.

## The question

An exchange is a data structure with rules. How fast can you run it, and can you prove you ran it correctly against a real day of messages?

## The answer

The Nasdaq TotalView-ITCH 5.0 sample for 30 December 2019 holds 268,744,780 messages for 8,906 stocks. On one pinned core Agora parses all of them and rebuilds every book in 34.2 seconds, the median of ten runs: 7.9 million messages a second, 127 ns a message. The runs ranged from 31.7 to 51.2 s, because other programs were using the machine for some of them; the fastest works out at 118 ns a message. Those figures are for a replay that looks 32 messages ahead. Applying one message at a time takes 58.5 s (218 ns a message), or 56.7 s with the hash map the first version used.

The day replays without one inconsistency. No message names an unknown order, none takes more shares than its order has left, and the 47,597,537,971 shares added balance exactly against the shares executed, cancelled, deleted and replaced, with nothing resting at the close.

![Latency percentiles by message type](results/latency.svg)

One message at a time, the median time to parse and apply a message is 173 ns, the 99th percentile 822 ns and the 99.9th 1.4 µs.

Two things in the day look like broken rules. The rebuilt books are crossed 30 times, always while the stock is halted or paused. And 10,565 of the 5,722,824 plain executions (0.185%) took an order that was not the oldest at its price. Every one of those hit an order that Nasdaq added to the book in a 40-second batch just after the open, and no other order ever jumped the queue. Why those orders rank where they do is my inference; the feed does not say.

The main limit is the timing. Other programs were running during the benchmark, using about 6.1 of the machine's 32 logical CPUs on average, and the spread between runs shows it. It is one machine and one day, and it compares this code with itself.

## Reproduce

```
scripts/fetch_data.sh        # 3.5 GB download into data/, once
scripts/reproduce.sh         # decompresses to 8.3 GB, then rebuilds results/
scripts/reproduce.sh --fast  # no download: the committed synthetic stream, a few seconds
```

On Windows use `scripts\fetch_data.ps1` and `scripts\reproduce.ps1`. You need stable Rust (`rust-toolchain.toml` pins 1.97.0) and `curl`. The full run needs about 12 GB of free disk and between one and two hours, most of it the ten timed rounds. `--runs 3` times three rounds instead of ten, `--runs 0` skips the timings, and `--cpu N` picks the core.

A second run gives the same counts, invariants, checkpoints and queue-priority tables byte for byte. The timing files change from run to run.

## What was built

- A price-time-priority limit order book (`src/book.rs`). Orders sit in a slab, linked into one first-in-first-out queue per price. Each side keeps its price levels in a sorted vector with the best price last. An order is named by a handle that carries a generation, so a handle kept past its order's life is refused.
- A matching engine on top of it (`src/engine.rs`): submit, partial cancel, delete, replace. An incoming order trades against the oldest order at the best opposite price first.
- A parser and encoder for all 23 message types of Nasdaq TotalView-ITCH 5.0 (`src/itch.rs`), and a reader for the length-prefixed sample files, plain or gzipped.
- An order reference index (`src/index.rs`): an open-addressing hash table whose slots can be prefetched. The standard hash map it replaced and a page table that was tried and lost are kept beside it, so the benchmark can time all three.
- A replay (`src/replay.rs`) that applies add, execute, cancel, delete and replace messages to one book per stock.
- An invariant checker inside the replay. After every book update it tests whether that book is crossed. For every execution it records whether the order was the oldest at the best price. At the end it audits every queue against its level totals and balances the share ledger.
- A queue-priority analysis (`src/priority.rs`) that takes each execution that skipped the queue and tests what could explain it.
- A benchmark: the day replayed ten times on a pinned core, a breakdown by stage, and latency percentiles per message type from the CPU's time-stamp counter (`src/hist.rs`). Criterion micro-benchmarks are in `benches/`.
- A seeded generator of valid ITCH streams (`src/synth.rs`). It drives the matching engine with random orders and writes what an exchange would publish, so replaying its output must rebuild the engine's books exactly.

The command is `agora`. `cargo run --release -- replay <file> --check` replays any ITCH 5.0 file and prints the invariant summary. `cargo run --release -- priority <file>` prints the queue-priority table, and `cargo run --release -- bench <file> --out <dir>` runs only the timings.

## How fast

All from `results/`, written by `scripts/reproduce`. The method and every pass are in `PAPER.md` under "How fast".

| Order index | Messages applied | Fastest | Median | Slowest | Median ns a message | Median million a second | Median against the first version |
|---|---|---:|---:|---:|---:|---:|---:|
| Standard hash map (the first version) | One at a time | 47.2 s | 56.7 s | 97.9 s | 211 | 4.74 | 1.00x |
| Page table | One at a time | 55.8 s | 73.2 s | 163.9 s | 272 | 3.67 | 0.77x |
| Open table | One at a time | 50.6 s | 58.5 s | 88.2 s | 218 | 4.59 | 0.97x |
| Standard hash map (the first version) | 32 at a time with prefetch | 33.1 s | 37.8 s | 56.6 s | 141 | 7.11 | 1.50x |
| Open table | 32 at a time with prefetch | 31.7 s | 34.2 s | 51.2 s | 127 | 7.85 | 1.66x |

One message at a time, neither index I wrote beats the standard hash map the first version used. On the medians the open table is 3% slower and the page table 29% slower. The page table loses because long-lived orders are scattered through the reference numbers and each one keeps a 64 KiB page alive: at 16:00 it holds 4.4 GB for 1,758,807 orders, where the open table holds 101 MB.

The gain comes from looking ahead. The batched replay parses 32 messages, asks the CPU to start fetching the index slots, orders and queue neighbours they will touch, and then applies them in order. That takes 34.2 s, 1.66 times as fast as the first version on the medians and 1.49 times on the fastest runs. Most of it comes from fetching the orders early. With the standard map, whose slots cannot be prefetched, batching still gets to 37.8 s. The books are the same either way.

Where the time goes, one message at a time on the open table: 18 ns to read and parse, 64 ns for the order lookup and 136 ns for the book update. Batching with prefetch cuts the last two together from 200 to 109 ns, so a large part of that time was spent waiting for memory.

Latency, the time to parse and apply one message, one at a time:

| Messages | p50 | p99 | p99.9 | p99.99 |
|---|---:|---:|---:|---:|
| Add Order (A) | 161 ns | 539 ns | 865 ns | 6.1 µs |
| Order Executed (E) | 153 ns | 694 ns | 951 ns | 6.1 µs |
| Order Cancel (X) | 59 ns | 614 ns | 951 ns | 5.6 µs |
| Order Delete (D) | 195 ns | 801 ns | 1.2 µs | 7.1 µs |
| Order Replace (U) | 368 ns | 1.3 µs | 2.3 µs | 16.2 µs |
| All messages | 173 ns | 822 ns | 1.4 µs | 6.9 µs |

The slowest single message took 25.3 ms. Every type is in `results/latency_percentiles.csv`. Timing every message adds 34 ns a message to the pass, measured as the timed pass minus the plain one. Two clock reads back to back take 5 ns, which understates it. The percentiles include whatever part of the cost falls between the two reads.

During the ten rounds other programs used about 6.1 of the machine's 32 logical CPUs on average, and rounds 5 to 8 caught the worst of it. The fastest column is the closest thing here to a quiet machine.

## Is it right

| | |
|---|---:|
| Messages in the day | 268,744,780 |
| Stocks in the directory | 8,906 |
| Orders added | 140,270,523 |
| Most orders resting at once | 1,924,078 |
| Messages naming an unknown order | 0 |
| Messages taking more shares than the order had | 0 |
| Shares added minus shares executed, cancelled, deleted, replaced and resting | 0 |
| Book updates that left a book crossed while the stock was trading | 0 |

Add Order is 43.6% of the messages, Order Delete 42.6% and Order Replace 8.1%. Executions are 2.2%. Only 1.5% of the shares added during the day were ever executed.

Every book is empty at the close, so a digest of the final books says nothing. `results/checkpoints.csv` has a digest of every book at eight times of day instead, from 07:00 to 18:00; at 13:00 there are 1,789,421 orders resting. The checked replay, the unchecked replay and the batched replay must agree on all eight or the run stops. The first version of the code, before any of the speed work, gives the same eight (`results/first_version/checkpoints.csv`).

The rebuilt books are checked against themselves. There is no public Nasdaq snapshot to compare them with.

## Executions out of queue order

Every one of the 5,722,824 Order Executed messages hit an order at the best price. 10,565 of them hit one that was not the oldest there. `agora priority` looks at each and writes `results/priority.csv`.

| Could it be | What the day shows |
|---|---|
| A minimum-quantity order skipping resting orders too small for it | Every skipped order was smaller than the fill in 3,600 of the 10,565, and not in the other 6,965 |
| Odd lots ranking behind round lots | Every skipped order was under 100 shares in 4,856 |
| My reading of Order Replace | 0 of the executed orders came from a replace, and 1 of the 88,133 skipped orders |
| Self-match prevention | 0 skipped orders share an attribution with the executed order. Only 73 executed orders were attributed, so this is a weak test |
| Halts or auctions | 0 happened outside trading state `T`, and none before 09:30 or after 16:00 |
| Orders added in the opening bursts | 10,565 of 10,565 |

The last row was found by looking at the data, so it is post hoc. In the 40 seconds after 09:30 the feed adds 944,183 orders in 45,586 bursts. A burst here is three or more Add Order messages in a row for one stock with one timestamp. They come about a millisecond apart, and there are none at any other time of day. Executions against burst orders were out of turn 10,565 times in 37,761. Executions against every other order were out of turn 0 times in 5,685,063.

One test could have broken this. In a second replay, each burst order is moved to the front of its queue the first time it trades out of turn. If those orders did not really rank ahead of what they skipped, something would later trade past them. 8,986 orders were moved and nothing was out of turn afterwards.

What the bursts are is inference. Nasdaq takes regular-hours orders before the open, and they cannot be on the displayed book until 09:30. If the bursts are those orders arriving, ranked by the time they were entered, the feed has no field that says so. `PAPER.md` has the sources and what is still open.

## What did not match the specification

- The book crosses during halts. 8,580 of 284,881,004 book updates left a book crossed, in 30 episodes across 13 stocks. All 30 began while the stock was halted or paused, when Nasdaq accepts orders without matching them. `results/crossed_examples.csv` lists them.
- The opening bursts above. The specification does not mention them.
- The specification does not say where a replaced order goes in the queue. Agora puts it at the back. The other reading I tried, keeping the place when the price is unchanged and the size is not larger, gives 11,153 out-of-turn executions, 588 of them with no burst to explain them.
- The file framing (a two-byte length before each message) is not in the specification. It was confirmed from the file.

## Data and sources

Nasdaq publishes full-day ITCH 5.0 sample files, free and without a login, at emi.nasdaq.com/ITCH. This project uses `12302019.NASDAQ_ITCH50.gz`, the smallest full day there (3.5 GB). The directory states no licence, so no file from it is committed here, and neither is anything derived from it beyond counts, timings, digests and the 13 symbols and 30 quotes in the crossed-book tables.

Nasdaq's checksum file for this day returns 404. The fetch script checks the size and the MD5 of the copy used here, which is weaker than the publisher's own checksum.

- Nasdaq, *Nasdaq TotalView-ITCH 5.0* specification, fetched 2026-11-23 from nasdaqtrader.com. `PAPER.md` records every message layout with its section number.
- Nasdaq, *Opening and Closing Crosses: Frequently Asked Questions*, fetched 2026-11-24 from nasdaqtrader.com. Used only for when regular-hours orders are accepted and executable.

The synthetic stream in `tests/data/` is generated by this code and is covered by the MIT licence.

## How it is tested

90 tests. `cargo test --release` runs them in about ten seconds.

- One test per message type (`tests/messages.rs`). Each builds a message by hand at the offsets in the specification, parses it, and checks that the encoder gives the same bytes back.
- Property tests (`tests/properties.rs`). The matching engine is compared, fill by fill, with a brute-force model over random order sequences; after every step the book is not crossed or locked, the queues pass a full audit, and shares in equal shares out. The parser round-trips every message in both directions and does not panic on arbitrary bytes. Replaying a generated stream rebuilds the generator's books for any seed. Replaying arbitrary, mostly invalid order messages still balances the ledger. All three order indexes agree with a plain hash map. A book driven with stale handles refuses them and stays sound. The prefetch hints and the choice of index change no book and no counter.
- Golden tests (`tests/golden.rs`). The committed synthetic stream, one million generated messages, and the first million messages of the real day each have a pinned digest of the full book state. The real-day test is skipped where the data file is absent, CI included.
- A test runs the smoke reproduce twice and compares both outputs with the committed `results/smoke/`.

CI runs `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, the smoke replay, and a check that the smoke tables did not change.

## Layout

```
src/               the order book, the matching engine, the parser, the index, the replay, the analyses
tests/             unit, property (proptest) and golden tests; a small synthetic stream
benches/           book and parser micro-benchmarks (criterion)
scripts/           fetch one sample day; reproduce the results
results/           committed tables and the chart
PAPER.md           the specification, what was checked against it, and how the timings were made
```

## Limits

- One day. Nothing here says how the numbers move on a busier day; the largest sample in the directory is five times this one.
- The timings are from one desktop CPU (Intel Core i9-13900K, Windows 11) on a pinned core, with other programs using about 6.1 CPUs. They say how this code compares with itself and should not be set beside a production engine's figures.
- The file is replayed from the operating system's cache. There is no network, no packet handling and no gap recovery. The batched replay looks 32 messages ahead, which a file always allows and a live feed allows only when messages arrive together.
- No sampling profiler was used. The stage breakdown comes from timing passes that each do one thing more than the last.
- The price-level structure, the parser and the allocator are the first versions written.
- The out-of-turn executions are explained by which orders they hit. Why Nasdaq ranks those orders so is inferred.
- Five message types (MWCB Status, Operational Halt, Broken Trade, RPII and DLCR Price Discovery) do not occur on this day, so their parsers have only been run on hand-built and generated messages.
- Hidden orders never appear in the feed, so the book is the displayed book only. Crosses are counted from their trade messages and are not simulated.
- The matching engine handles limit orders only: no market orders, no time-in-force, no hidden or pegged orders, no auctions.
- It replays an exchange's book. It is not connected to anything and sends no orders.
- This is research code. It is not investment advice and it does not trade.

## Licence

MIT. See `LICENSE`.
