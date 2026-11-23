# The specification, and what was checked against it

## Source

Nasdaq, *Nasdaq TotalView-ITCH 5.0* (the specification PDF, 36 pages, last revision entry 28 April 2023), fetched on 2026-11-23 from
`https://www.nasdaqtrader.com/content/technicalsupport/specifications/dataproducts/NQTVITCHspecification.pdf`.

Section numbers below are the ones printed in that PDF. Its revision log still refers to some sections by older numbers (for example "4.2.8" for the Operational Halt message, which the body numbers 1.2.8); this file uses the body numbers.

The PDF was read in full before the parser was written. Every layout below was copied from its tables and is implemented in `src/itch.rs`. `tests/messages.rs` builds one message of every type by hand at these offsets and parses it.

## Data types (section "Data Types", page 4)

- Integers are big-endian and unsigned.
- Alpha fields are ASCII, left justified, padded on the right with spaces.
- Prices are integers with an implied number of decimals. Price(4) has four decimals; the largest Price(4) is 200,000.0000, which is 0x77359400. Price(8) has eight.
- Timestamps are nanoseconds since midnight, in six bytes.

The code keeps prices as the integer on the wire and alpha fields as raw bytes. Nothing is converted to floating point.

## Common header

Every message starts with the same four fields.

| Offset | Length | Type | Field |
|---:|---:|---|---|
| 0 | 1 | Alpha | Message type |
| 1 | 2 | Integer | Stock locate (0 when the message is not about one stock) |
| 3 | 2 | Integer | Tracking number |
| 5 | 6 | Integer | Timestamp, nanoseconds since midnight |

## Message layouts

Offsets and lengths are in bytes. Only the fields after the common header are listed.

### Messages that change a book

| Type | Section | Name | Bytes | Fields after the header (offset, length) |
|---|---|---|---:|---|
| `A` | 1.3.1 | Add Order, no MPID | 36 | order reference (11, 8); buy/sell `B` or `S` (19, 1); shares (20, 4); stock (24, 8); price, Price(4) (32, 4) |
| `F` | 1.3.2 | Add Order with MPID | 40 | as `A`, then attribution (36, 4) |
| `E` | 1.4.1 | Order Executed | 31 | order reference (11, 8); executed shares (19, 4); match number (23, 8) |
| `C` | 1.4.2 | Order Executed With Price | 36 | order reference (11, 8); executed shares (19, 4); match number (23, 8); printable `Y` or `N` (31, 1); execution price, Price(4) (32, 4) |
| `X` | 1.4.3 | Order Cancel | 23 | order reference (11, 8); cancelled shares (19, 4) |
| `D` | 1.4.4 | Order Delete | 19 | order reference (11, 8) |
| `U` | 1.4.5 | Order Replace | 35 | original order reference (11, 8); new order reference (19, 8); shares (27, 4); price, Price(4) (31, 4) |

How each one is applied, and the part of the specification it rests on:

- `A`, `F`: the order joins the back of the queue at its price. Section 1.3 says an add order has been accepted and added to the displayable book.
- `E`, `C`: the executed shares come off the order, which keeps its place. Section 1.4 says modify messages are cumulative and that an order whose display shares reach zero must be removed from the book. The execution price in `C` is the price of the trade and does not move the order.
- `X`: the cancelled shares come off the order, which keeps its place (section 1.4.3 calls this a partial cancellation).
- `D`: the order is removed (section 1.4.4).
- `U`: the old order is removed and a new one, with the new reference, shares and price, joins the back of the queue at the new price. Section 1.4.5 says the side, stock and attribution are those of the original order. The specification does not say where in the queue the replacement goes. Putting it at the back is this project's reading, and the full-day result below is the evidence for it.

### Trade messages (no book change)

| Type | Section | Name | Bytes | Fields after the header (offset, length) |
|---|---|---|---:|---|
| `P` | 1.5.1 | Trade (Non-Cross) | 44 | order reference (11, 8); buy/sell (19, 1); shares (20, 4); stock (24, 8); price, Price(4) (32, 4); match number (36, 8) |
| `Q` | 1.5.2 | Cross Trade | 40 | shares (11, 8); stock (19, 8); cross price, Price(4) (27, 4); match number (31, 8); cross type (39, 1) |
| `B` | 1.5.3 | Broken Trade | 19 | match number (11, 8) |

Section 1.5.1 says a `P` message reports a match against an order that was never displayed, so it has no order on the book to change. Section 1.5.3 says a book builder may ignore `B`.

### Administrative messages (no book change)

| Type | Section | Name | Bytes | Fields after the header (offset, length) |
|---|---|---|---:|---|
| `S` | 1.1 | System Event | 12 | event code (11, 1) |
| `R` | 1.2.1 | Stock Directory | 39 | stock (11, 8); market category (19, 1); financial status (20, 1); round lot size (21, 4); round lots only (25, 1); issue classification (26, 1); issue sub-type (27, 2); authenticity (29, 1); short sale threshold (30, 1); IPO flag (31, 1); LULD reference price tier (32, 1); ETP flag (33, 1); ETP leverage factor (34, 4); inverse indicator (38, 1) |
| `H` | 1.2.2 | Stock Trading Action | 25 | stock (11, 8); trading state (19, 1); reserved (20, 1); reason (21, 4) |
| `Y` | 1.2.3 | Reg SHO Restriction | 20 | stock (11, 8); Reg SHO action (19, 1) |
| `L` | 1.2.4 | Market Participant Position | 26 | MPID (11, 4); stock (15, 8); primary market maker (23, 1); market maker mode (24, 1); participant state (25, 1) |
| `V` | 1.2.5.1 | MWCB Decline Level | 35 | level 1, Price(8) (11, 8); level 2 (19, 8); level 3 (27, 8) |
| `W` | 1.2.5.2 | MWCB Status | 12 | breached level (11, 1) |
| `K` | 1.2.6 | IPO Quoting Period Update | 28 | stock (11, 8); quotation release time, seconds since midnight (19, 4); release qualifier (23, 1); IPO price, Price(4) (24, 4) |
| `J` | 1.2.7 | LULD Auction Collar | 35 | stock (11, 8); reference price (19, 4); upper collar (23, 4); lower collar (27, 4); extension (31, 4) |
| `h` | 1.2.8 | Operational Halt | 21 | stock (11, 8); market code (19, 1); halt action (20, 1) |
| `I` | 1.6 | NOII | 50 | paired shares (11, 8); imbalance shares (19, 8); imbalance direction (27, 1); stock (28, 8); far price (36, 4); near price (40, 4); current reference price (44, 4); cross type (48, 1); price variation indicator (49, 1) |
| `N` | 1.7 | RPII | 20 | stock (11, 8); interest flag (19, 1) |
| `O` | 1.8 | DLCR Price Discovery | 48 | stock (11, 8); open eligibility (19, 1); minimum price (20, 4); maximum price (24, 4); near execution price (28, 4); near execution time (32, 8); lower collar (40, 4); upper collar (44, 4) |

The replay reads two of these. `R` gives the symbol for a stock locate. `H` gives the trading state (`H` halted, `P` paused, `Q` quotation only, `T` trading), which the invariant checker uses to say whether a crossed book was halted at the time.

## What is not in the specification

The file framing. Nasdaq's sample files hold the day's messages back to back, each preceded by its length as a two-byte big-endian integer. The ITCH specification describes messages only; it names SoupBinTCP and MoldUDP64 as the transports and does not describe a file format. The two-byte length prefix was confirmed by reading the file: with that framing every one of its messages has a known type and exactly the length the specification gives for that type (see the results below). No Nasdaq document for the file format was read.

## The sample day

`12302019.NASDAQ_ITCH50.gz` from `https://emi.nasdaq.com/ITCH/Nasdaq%20ITCH/`, Monday 30 December 2019. It was the smallest full day in the directory on 2026-11-23: 3,524,013,057 bytes compressed, 8,251,407,909 bytes decompressed.

The directory lists a `.md5sum` file beside it, but requesting that file returns HTTP 404, so the download could not be checked against Nasdaq's checksum. Two weaker checks passed: the size equals the directory listing, and the gzip stream decompressed to the end with its own CRC intact. The MD5 of the copy used here is `23514ef98821448a3f8f4c5b4095f3f5`; `scripts/fetch_data` compares against it.

The file is older than three changes in the specification's revision log: the Extended Trading Close cross type (January 2022), non-integer ETP leverage factors (July 2022), and the DLCR message and the "paused" imbalance direction (April 2023). None of them changes the layout of a message that is in the file.

## Claim by claim

Every number is from `results/`, written by `agora reproduce` in this build.

| What the specification says | What the day shows |
|---|---|
| Messages have the types and lengths in sections 1.1 to 1.8. | All 268,744,780 messages parse. No unknown type, no wrong length, no buy/sell indicator other than `B` or `S`. |
| The first message of the day is the start-of-messages system event and the last is the end-of-messages event (section 1.1). | First message at 03:04:32.057543747, last at 20:05:00.000040070. Six system events in the day. |
| Messages are sequenced. | No timestamp is lower than the one before it. |
| The order reference number is unique for the day (section 1.3). | 140,270,523 orders added; no reference was added twice. |
| Modify messages name an order that was added (section 1.4). | No execute, cancel, delete or replace names an unknown reference, and none carries a stock locate different from the order's. |
| Executed and cancelled shares come off the remaining shares; at zero the order is dead (section 1.4). | No message takes more shares than its order has left. |
| A replacement keeps the side and stock of the original (section 1.4.5). | Applied as written; the specification gives no field to check it against. |
| `W`, `h`, `B`, `N` and `O` exist. | None appears in this day, so those five parsers are tested on hand-built and generated messages only. |

Counts by type are in `results/messages_by_type.csv`. Add Order (`A`) is 43.6% of the day, Order Delete (`D`) 42.6%, Order Replace (`U`) 8.1%.

## The two invariants

### Volume is conserved

Every share added leaves by one of four routes or is still resting at the end.

| Route | Shares |
|---|---:|
| Added by `A`, `F` and `U` | 47,597,537,971 |
| Executed by `E` and `C` | 711,223,674 |
| Cancelled by `X` | 1,164,422,563 |
| Deleted by `D` | 39,631,356,897 |
| Still on the old order when `U` replaced it | 6,090,534,837 |
| Resting at the end of the day | 0 |
| Added minus all of the above | 0 |

The resting total is read from the books at the end, after an audit that walks every queue and compares it with its level's aggregate, so the balance is a check on the book structure and not only on the arithmetic. Every book is empty at the end of the day: each order that was added was later executed, deleted or replaced. The largest number of orders resting at once was 1,924,078.

Only 1.5% of the shares added were executed.

### The book is never crossed, with exceptions

After each of the 284,881,004 book updates the checker compares that stock's best bid and best ask. (A replace counts as two updates.)

- 8,580 updates left a book crossed, in 30 separate episodes across 13 stocks.
- All 30 episodes began while the stock's last Trading Action state was `H` (halted, 6 episodes) or `P` (paused, 24). None began while the state was `T`.
- 70 updates left a book locked (best bid equal to best ask). The checker does not record the trading state for these.
- No book was crossed at the end of the day.

So the claim that holds on this day is narrower than the design's: the rebuilt book is never crossed while the stock is trading. During a halt or pause Nasdaq keeps accepting orders without matching them, and the displayed book crosses until the reopening cross. The specification does not state this. It is what the data shows, and it agrees with how section 1.2.2 describes halts and section 1.6 describes the halt cross. `results/crossed_examples.csv` lists all 30 episodes and `results/crossed_symbols.csv` the 13 stocks. PNRL was crossed longest: 43,149 seconds, from 08:00:50 until about 20:00.

## Price-time priority

The feed does not say why an order was executed, but the rebuilt book can say where the order was. For each execution the checker records whether the order was at the best price on its side and whether it was the oldest order there.

| | `E` Order Executed | `C` Order Executed With Price |
|---|---:|---:|
| Executions | 5,722,824 | 99,917 |
| At the best price | 5,722,824 | 99,644 |
| Oldest order at the best price | 5,712,259 | 17,268 |

Every `E` execution was at the best price, and 99.815% took the oldest order there. The other 10,565 took an order at the best price that had an older order ahead of it in the rebuilt queue. All 10,565 happened between 09:30 and 16:00 with the stock in state `T`. I could not determine the cause. Two candidates, neither tested: order types whose priority is not the order of their add messages, and a queue rule for Order Replace that differs from both readings tried below.

Section 1.4.2 says `C` is sent when the execution price differs from the display price, and that non-printable ones belong to a later bulk print such as a cross. Only 17% of them took the oldest order at the best price, so queue order is not what decides them. 3,754,970 of their shares are marked non-printable.

## Differences from the source

- The framing of the file is not in the specification (see "What is not in the specification").
- The book is crossed during halts and pauses. The design promised a book that never crosses.
- 10,565 of 5,722,824 `E` executions (0.185%) do not follow the price-time order of the rebuilt queue.
- Where a replaced order goes in the queue is this project's reading. The specification is silent.
- The design called for a golden test on the first million messages of a sample day. That test exists and passes here (`tests/golden.rs`, digest `8b546eda1fe37dee`), but it is skipped wherever the data file is absent, which includes CI. CI runs the same kind of test on the committed synthetic stream and on one million generated messages.

## Variants tried

Two things were tried in more than one way. Nothing else was tuned.

1. Queue position after Order Replace, two readings, each run once on the full day:

   | Reading | `E` executions not taking the oldest order |
   |---|---:|
   | Every replacement goes to the back of the queue (used for all results) | 10,565 |
   | A replacement at the same price for no more shares keeps its place | 11,153 |

   The first reading fits better and is the default. The second is still in the code (`agora replay --check --replace-keeps-priority`) so the comparison can be rerun. Both readings give the same final books, crossed counts and ledger.

2. The full reproduce was run twice. Throughput moved between runs on this shared machine:

   | Run | Three timed replays (seconds) | Median |
   |---|---|---:|
   | First, before the replace comparison was added | 55.740, 48.202, 47.180 | 48.202 |
   | Second, the one committed in `results/` | 54.649, 54.905, 49.086 | 54.649 |

   The README quotes the committed run. The data structures were not changed between the two runs.

No performance variants were tried. The book, the order map and the reader are the first versions written.

## How the timing was measured

- Machine: Intel Core i9-13900K, 64 GB, Windows 11, Rust 1.97.0, release profile with fat LTO and one codegen unit. The process is single-threaded and was not pinned to a core. Other programs were running.
- The timed replay reads the decompressed file from disk through a 4 MiB buffer, parses every message and applies it. The file was in the operating system's cache: a parse-only pass runs twice first. Reading and parsing alone take 4.9 s of the 54.6 s.
- Per-message latency is the time-stamp counter read before parsing a message and after applying it, in a separate pass. Ticks are converted to nanoseconds by comparing the counter with the system clock over the whole pass. An empty measurement costs about 5 ns (median) and is not subtracted. Percentiles come from a histogram with buckets about 3% wide; the maximum is exact.
- The per-message numbers measure one message with the next one not yet parsed. They are the cost of this code on a cache-warm file, and say nothing about a network feed.

## Could not verify

- The download against Nasdaq's own checksum (404, above).
- The licence of the sample files. The directory states none.
- The cause of the 10,565 out-of-order `E` executions.
- That the rebuilt books equal Nasdaq's own. There is no public snapshot to compare with. The evidence is internal: no message contradicts the book it is applied to, volume balances to zero, and every book is empty at the end.
- The parsers for `W`, `h`, `B`, `N` and `O` against real messages.
- CI. The workflow is committed; whether it ran is in the hand-over note, not here.
