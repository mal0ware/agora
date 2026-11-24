# The specification, and what was checked against it

## Source

Nasdaq, *Nasdaq TotalView-ITCH 5.0* (the specification PDF, 36 pages, last revision entry 28 April 2023), fetched on 2026-11-23 from
`https://www.nasdaqtrader.com/content/technicalsupport/specifications/dataproducts/NQTVITCHspecification.pdf`.

Section numbers below are the ones printed in that PDF. Its revision log still refers to some sections by older numbers (for example "4.2.8" for the Operational Halt message, which the body numbers 1.2.8); this file uses the body numbers.

Every layout below was copied from its tables and is implemented in `src/itch.rs`. `tests/messages.rs` builds one message of every type by hand at these offsets and parses it.

One other document is used, only in the section on queue priority: Nasdaq, *Opening and Closing Crosses: Frequently Asked Questions*, fetched on 2026-11-24 from
`https://www.nasdaqtrader.com/content/productsservices/Trading/Crosses/openclose_faqs.pdf`.

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

Every number is from `results/`, written by `agora reproduce`.

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

## Invariants

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

The resting total is read from the books at the end, after an audit that walks every queue and compares it with its level's aggregate, so the balance also checks the book structure. Every book is empty at the end of the day: each order that was added was later executed, deleted or replaced. The largest number of orders resting at once was 1,924,078.

Only 1.5% of the shares added were executed.

### Book fingerprints through the day

Every book is empty at the end, so a digest of the final books is the digest of nothing. The run takes a digest of every book just before the first message at or after each of eight times instead. The digest covers each level's price, total and order count and each order's reference and remaining shares, in queue order.

| Time of day | Messages before | Orders resting | Digest of all books |
|---|---:|---:|---|
| 07:00:00 | 2,524,145 | 7,574 | `4af5c11b1686ce4c` |
| 09:30:00 | 6,696,719 | 414,434 | `db2caccc980653e3` |
| 09:35:00 | 12,832,456 | 1,653,863 | `93d646146a2b0e8d` |
| 11:00:00 | 94,229,269 | 1,756,394 | `aebbad60de993a00` |
| 13:00:00 | 164,067,379 | 1,789,421 | `835aeb97a4a0c8e1` |
| 15:00:00 | 219,952,342 | 1,789,757 | `baaa72f5aad3b65b` |
| 16:00:00 | 265,920,781 | 1,758,807 | `b163ab709d83e008` |
| 18:00:00 | 268,397,172 | 147,778 | `ed4d97569cef74d6` |

The checked replay, the unchecked replay and the batched replay are each run to these points and must give the same eight digests, or the run stops. The table is `results/checkpoints.csv`. It also holds the digest after the first million messages, `8b546eda1fe37dee`, which `tests/golden.rs` pins.

The first version of the code (commit `5b7fc48`, before the order index, the handles and the batching were changed) gives the same nine digests. Its table is kept as `results/first_version/checkpoints.csv`; `git checkout 5b7fc48` and `cargo run --release -- reproduce --runs 1` rebuilds it.

### The book is never crossed, with exceptions

After each of the 284,881,004 book updates the checker compares that stock's best bid and best ask. (A replace counts as two updates.)

- 8,580 updates left a book crossed, in 30 separate episodes across 13 stocks.
- All 30 episodes began while the stock's last Trading Action state was `H` (halted, 6 episodes) or `P` (paused, 24). None began while the state was `T`.
- 70 updates left a book locked (best bid equal to best ask). The checker does not record the trading state for these.
- No book was crossed at the end of the day.

So the claim that holds on this day is that the rebuilt book is never crossed while the stock is trading. During a halt or pause Nasdaq keeps accepting orders without matching them, and the displayed book crosses until the reopening cross. The specification does not state this. It is what the data shows, and it agrees with how section 1.2.2 describes halts and section 1.6 describes the halt cross. `results/crossed_examples.csv` lists all 30 episodes and `results/crossed_symbols.csv` the 13 stocks. PNRL was crossed longest: 43,149 seconds, from 08:00:50 until about 20:00.

## Price-time priority

The feed does not say why an order was executed, but the rebuilt book can say where the order was. For each execution the checker records whether the order was at the best price on its side and whether it was the oldest order there.

| | `E` Order Executed | `C` Order Executed With Price |
|---|---:|---:|
| Executions | 5,722,824 | 99,917 |
| At the best price | 5,722,824 | 99,644 |
| Oldest order at the best price | 5,712,259 | 17,268 |

Every `E` execution was at the best price, and 99.815% took the oldest order there. The other 10,565 took an order that had an older order ahead of it in the rebuilt queue. Call these out of turn.

Section 1.4.2 says `C` is sent when the execution price differs from the display price, and that non-printable ones belong to a later bulk print such as a cross. Only 17% of them took the oldest order at the best price, so queue order is not what decides them. 3,754,970 of their shares are marked non-printable.

### What the out-of-turn executions have in common

`agora priority <file>` looks at each of the 10,565 and writes `results/priority.csv` and `results/priority_by_time.csv`. It first tests the explanations that need nothing but the feed.

| Explanation | What it predicts | What the day shows |
|---|---|---|
| A minimum-quantity incoming order skipped resting orders that were too small for it | Every skipped order is smaller than the shares executed | True for 3,600 of 10,565 (34%). In the other 6,965 at least one skipped order was big enough |
| Odd lots rank behind round lots | Every skipped order is under 100 shares | True for 4,856 (46%) |
| The queue place of replaced orders is wrong | The executed order, or a skipped one, came from an Order Replace | 0 executed orders and 1 of 88,133 skipped orders came from a replace. None came from a replace that kept its price without growing, which is the only case the two readings place differently |
| Self-match prevention | A skipped order carries the same attribution as the executed one | 0. Only 73 of the executed orders were attributed at all, so the feed can hardly test this |
| Halts | The stock was not in state `T` | 0 |
| A different message type | The executions are `C`, or non-printable | All are `E` by construction, and `E` has no printable flag. `C` is counted apart, above |
| Auctions and time of day | They sit at the open or the close | 9,052 (86%) fall between 09:30 and 10:00, and 108 in the last half-hour. None before 09:30 or after 16:00. They are spread over 436 stocks |

None of these explains the set. Most skipped orders traded soon after. Of the 88,133 times an order was skipped, its next message was an execution in the same nanosecond 36,499 times, a later execution 25,707 times, a delete 25,846 times and a partial cancel 81 times.

### The opening bursts

This part was found by looking at the 10,565 rows, so it is post hoc.

In the 40 seconds after 09:30 the feed carries bursts of Add Order messages: many adds for one stock, back to back, with one timestamp. Define a burst as three or more consecutive add messages (`A` or `F`) with the same stock and the same timestamp. The threshold of three was chosen after seeing the data. It matters little: the shortest burst behind any out-of-turn execution has 12 messages.

| | |
|---|---:|
| Bursts in the day | 45,586 |
| Orders added in bursts | 944,183 |
| Stocks with a burst | 8,818 of 8,906 |
| Longest burst | 90 messages |
| First burst | 09:30:00.000995 |
| Last burst | 09:30:39.737184 |
| Bursts at any other time of day | 0 |

The bursts arrive about one a millisecond for 40 seconds and never again. That pattern is a machine loading a batch of orders into the book. Executions split on it:

| | Executions (`E`) | Out of turn |
|---|---:|---:|
| Against an order added in a burst | 37,761 | 10,565 (28.0%) |
| Against any other order | 5,685,063 | 0 |

Every out-of-turn execution hit a burst order, and every order it skipped was an ordinary one. In each case the burst order was added after its own stock's opening cross message. The 10,565 executions involve 8,986 distinct orders in 436 stocks. All but one of the executions were in Nasdaq-listed stocks.

On this day an order that entered the book in an opening burst can rank ahead of orders whose add messages came earlier, and an ordinary order never does.

### A test that could have failed

If burst orders really sit ahead of what they skipped, the damage should be one-off: once the rebuilt queue is corrected for an order, nothing further should be out of turn. The analysis runs a second replay beside the first. In it, the first time a burst order is executed out of turn it is moved to the front of its queue. Then it counts what is still out of turn.

| Second replay | |
|---|---:|
| Burst orders moved to the front | 8,986 |
| Executed out of turn again after the move | 0 |
| Out-of-turn executions of an order that is not a burst order | 0 |

Had a moved order been behind something after all, the order it jumped would later have traded past it and shown up in the last two rows. Nothing did, across 5.7 million executions.

### What the bursts probably are

The feed does not say, and no Nasdaq document I read states it, so this is inference.

Nasdaq accepts orders before the open that cannot trade until regular hours. Its Opening and Closing Crosses FAQ says regular-hours orders are executable from 09:30 to 16:00, and lists order types that are accepted hours earlier. The ITCH specification's Start of Market Hours system event (section 1.1) says it marks the point where market-hours orders become available for execution. Such an order is not on the displayed book before 09:30, so the feed cannot show it until then. The same FAQ says of one order type that is re-priced at the open that it keeps its original time of entry for time priority.

If the bursts are those held orders reaching the book, and they keep the time they were entered, then a burst order entered at 08:00 belongs ahead of a displayed order added at 09:00, although its add message came later. An add message carries the time it was sent and nothing else, so no book built from this feed can place those orders correctly in advance. That fits every count above.

### What is still not explained

- The cause is inferred. The counts show which orders jump the queue. They do not show why Nasdaq ranks them so.
- Not every burst order jumps. 27,196 executions of burst orders were in turn. On the reading above those orders were entered later than what rested ahead of them, but the feed cannot confirm it.
- The move-to-front rule is a repair after the fact. It shows the anomalies are consistent with one hidden ranking, without recovering that ranking.
- One day. Another day might show out-of-turn executions that bursts do not cover.

## Differences from the source

- The framing of the file is not in the specification (see "What is not in the specification").
- The book is crossed during halts and pauses. The specification does not say so.
- 10,565 of 5,722,824 `E` executions (0.185%) do not follow the price-time order of the rebuilt queue. All of them hit orders added in the opening bursts. The specification does not describe those bursts or how such orders rank.
- Where a replaced order goes in the queue is this project's reading. The specification is silent.
- The golden test on the first million messages of the real day (`tests/golden.rs`, digest `8b546eda1fe37dee`) is skipped wherever the data file is absent, which includes CI. CI runs the same kind of test on the committed synthetic stream and on one million generated messages.

## Variants tried

Everything that was tried in more than one way is listed here, including what did not help.

1. Queue position after Order Replace, two readings, each run on the full day:

   | Reading | `E` executions out of turn | Of those, not explained by an opening burst |
   |---|---:|---:|
   | Every replacement goes to the back of the queue (used for all results) | 10,565 | 0 |
   | A replacement at the same price for no more shares keeps its place | 11,153 | 588 |

   The first reading is the default. The second is still in the code (`agora replay --check --replace-keeps-priority`, `agora priority --replace-keeps-priority`) so the comparison can be rerun. Both give the same final books, crossed counts and ledger.

2. The order reference index, three ways. Each benchmark round times all three, and the property tests check that all three build the same books.

   - The standard hash map with a one-multiply hasher. This is what the first version used.
   - A page table indexed by the reference number itself (`PageIndex`). Nasdaq hands out reference numbers in rising order, so new orders would sit next to each other in memory. It was slower. Orders that rest for hours are scattered through the number range and each one keeps a 64 KiB page alive, so the table grows to 4.4 GB to hold under two million orders.
   - An open-addressing hash table with linear probing (`OpenIndex`), now the default. At its largest it holds 101 MB, against an estimated 92 MB for the standard map. Its use is that the slot a reference will land in is known from the reference alone, so it can be prefetched.

   What each index holds through the day (`results/index_memory.csv`):

| Time of day | Orders resting | Standard hash map (estimate) | Page table | Open table |
|---|---:|---:|---:|---:|
| 07:00:00 | 7,574 | 0 MB | 50 MB | 0 MB |
| 09:30:00 | 414,434 | 11 MB | 153 MB | 25 MB |
| 09:35:00 | 1,653,863 | 92 MB | 262 MB | 101 MB |
| 11:00:00 | 1,756,394 | 92 MB | 1,800 MB | 101 MB |
| 13:00:00 | 1,789,421 | 92 MB | 3,065 MB | 101 MB |
| 15:00:00 | 1,789,757 | 92 MB | 4,060 MB | 101 MB |
| 16:00:00 | 1,758,807 | 92 MB | 4,382 MB | 101 MB |
| 18:00:00 | 147,778 | 92 MB | 1,348 MB | 101 MB |

3. Batching with prefetch. The replay can parse 32 messages ahead, tell the CPU which index slots, orders and queue neighbours they will touch, and then apply them in order. The books are the same; the property tests and the checkpoint digests check that. Batch sizes of 8, 16, 32, 64 and 128 were each run twice on a busy machine and could not be told apart, so 32 stayed. A further hint for add messages (the slot the order will take and the order it will queue behind) showed no gain in the same rough test and was removed.

4. Not tried: a different price-level structure, a different parser, or a custom allocator. The stage breakdown below says where they would have to help.

## How fast

All numbers are from `results/`, written by `agora reproduce` (which ends by running `agora bench`).

### Method

- The benchmark thread is pinned to one logical CPU (number 2, a performance core on this machine) and raised to the highest thread priority.
- Warm-up, not timed: one parse pass to pull the file into the operating system's cache, then one full replay.
- Then ten rounds. Each round runs every pass once, in the same order, so drift on the machine lands on all of them alike. The tables give the minimum, median and maximum over the ten rounds.
- Every timed replay must end with the same message, order and share counts as the checked pass, or the run stops.
- The run reads machine-wide CPU time before and after the rounds and reports how much work everything else on the machine did in that time.
- Machine: Intel Core i9-13900K, 64 GB, Windows 11, Rust 1.97.0, release profile with fat LTO and one codegen unit. The file is read from the operating system's cache through a 4 MiB buffer.

The machine was not quiet. Other programs used about 6.1 of its 32 logical CPUs over the timed rounds. Rounds 5 to 8 ran while they were busiest, and the replay passes took up to twice as long in those rounds as in the others. Pinning and priority keep the benchmark thread on its core, but they do not stop other programs from competing for memory and cache, and this workload is mostly memory. The minimum of each pass is the best estimate of what the code costs; the median is what this machine delivered that hour.

### The day, ten times

| Pass | Fastest | Median | Slowest | ns a message (median) |
|---|---:|---:|---:|---:|
| Read the file and split it into messages | 1.8 s | 1.9 s | 2.8 s | 7 |
| Read and parse | 4.6 s | 4.8 s | 6.6 s | 18 |
| Read, parse, order lookups only: standard hash map | 16.0 s | 18.4 s | 33.8 s | 69 |
| Read, parse, order lookups only: page table | 12.5 s | 15.8 s | 25.3 s | 59 |
| Read, parse, order lookups only: open table | 21.0 s | 22.0 s | 31.2 s | 82 |
| Replay, standard hash map (the first version) | 47.2 s | 56.7 s | 97.9 s | 211 |
| Replay, page table | 55.8 s | 73.2 s | 163.9 s | 272 |
| Replay, open table | 50.6 s | 58.5 s | 88.2 s | 218 |
| Replay in batches of 32 with prefetch, standard hash map | 33.1 s | 37.8 s | 56.6 s | 141 |
| Replay in batches of 32 with prefetch, open table | 31.7 s | 34.2 s | 51.2 s | 127 |
| Replay with every check on, open table | 53.1 s | 61.9 s | 87.9 s | 230 |
| Replay with per-message timing, open table | 58.6 s | 67.5 s | 98.1 s | 251 |

`results/throughput_runs.csv` has each of the ten timings of every pass.

### Before and after

The same rounds time the three order indexes one message at a time, and two of them in batches of 32 with prefetch.

| Order index | Messages applied | Fastest | Median | Slowest | Median ns a message | Median million a second | Median against the first version |
|---|---|---:|---:|---:|---:|---:|---:|
| Standard hash map (the first version) | One at a time | 47.2 s | 56.7 s | 97.9 s | 211 | 4.74 | 1.00x |
| Page table | One at a time | 55.8 s | 73.2 s | 163.9 s | 272 | 3.67 | 0.77x |
| Open table | One at a time | 50.6 s | 58.5 s | 88.2 s | 218 | 4.59 | 0.97x |
| Standard hash map (the first version) | 32 at a time with prefetch | 33.1 s | 37.8 s | 56.6 s | 141 | 7.11 | 1.50x |
| Open table | 32 at a time with prefetch | 31.7 s | 34.2 s | 51.2 s | 127 | 7.85 | 1.66x |

One message at a time, the standard hash map is the fastest of the three. On the medians the open table is 3% slower and the page table 29% slower; on the fastest runs the open table is 7% slower. In the busy rounds the order flips and the open table is ahead, which I have not explained.

Batching with prefetch is what helps: 1.66x on the medians with the open table and 1.50x with the standard map, whose slots cannot be prefetched. So most of the gain is from fetching orders and queue neighbours early, and the open table adds the rest. On the fastest run of each pass the two ratios are 1.49x and 1.43x.

The first version as committed (`c13e4bc`) reported 54.6 s, the median of three unpinned runs. The standard-hash-map row is that structure in today's code, which also carries the generation check on every handle.

### Where the time goes

No sampling profiler was used. The breakdown comes from passes that each do one thing more than the last: read the file and split it into messages; also parse; also do the order reference lookups a replay would do, with no book; the whole replay. The differences between their medians give the stages.

| Stage, ns a message | Standard hash map | Page table | Open table |
|---|---:|---:|---:|
| Read the file and split it into messages | 7 | 7 | 7 |
| Parse | 11 | 11 | 11 |
| Order reference lookup | 51 | 41 | 64 |
| Book update and counters | 142 | 214 | 136 |
| Whole replay | 211 | 272 | 218 |

Reading and parsing are 18 ns of a message. The rest is the order lookup and the book update, and the split between those two is rough: the lookup-only pass has no book competing for cache, so it flatters whichever index uses more memory. The page table shows this. Its lookups look cheapest alone (41 ns) and its book stage is the dearest (214 ns), because its gigabytes push the books out of cache.

Batching with prefetch takes the open table's lookup and book stages together from 200 to 109 ns. Prefetch changes nothing but when memory is fetched, so about that much of the time was waiting for memory.

### What per-message timing costs

Per-message latency is the time-stamp counter read before parsing a message and after applying it, in a separate pass, one message at a time. Ticks are converted to nanoseconds by comparing the counter with the system clock over the whole pass. Percentiles come from a histogram whose buckets are 1/32 of their value wide. A percentile is the middle of its bucket, so it is within 1.6% of the true value; values under 64 ticks (21 ns) and the maximum are exact.

| | |
|---|---:|
| Replay, one message at a time | 218 ns a message |
| The same with a clock read before and after every message | 251 ns a message |
| What timing adds to the pass | 34 ns a message |
| Mean of the per-message measurements | 227 ns |
| Two clock reads back to back, median | 5 ns |

Timing every message adds 34 ns a message to the pass. Two clock reads back to back take 5 ns, but that figure leaves out what the reads do to the code around them and the cost of recording the result. The percentiles include the part of the cost that falls between the two reads, so they run a little high; the mean of the measurements is 227 ns against 218 ns a message for the untimed replay.

The per-message numbers time one message with the next one not yet parsed. They are the cost of this code on a cache-warm file, and say nothing about a network feed.

## Could not verify

- The download against Nasdaq's own checksum (404, above).
- The licence of the sample files. The directory states none.
- Why opening-burst orders rank ahead of orders with earlier add messages. The counts are firm; the cause is inferred.
- That the rebuilt books equal Nasdaq's own. There is no public snapshot to compare with. The evidence is internal: no message contradicts the book it is applied to, volume balances to zero, and every book is empty at the end.
- The parsers for `W`, `h`, `B`, `N` and `O` against real messages.
- The timings on a quiet machine. Other programs used about 6.1 CPUs during the benchmark and I could not stop them.
