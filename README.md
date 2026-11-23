# Agora

**A limit order book in Rust that rebuilds a full Nasdaq trading day from the exchange's own messages.**

> **Status: planned.** This repository holds the design. No results exist yet, and nothing below is a finding. Each box in the roadmap gets ticked when the code and its tests are in.

## The question

An exchange is a data structure with rules. How fast can you run it, and can you prove you ran it correctly against a real day of messages?

## What this will contain

- A price-time-priority limit order book: add, cancel, replace, execute
- A parser for Nasdaq TotalView-ITCH 5.0
- A replay tool that rebuilds the book for every symbol across one full trading day
- A benchmark: messages per second, and latency percentiles per message type
- A checker that the rebuilt best bid and offer never cross and that volume is conserved

## The result it is built to show

One line: the number of messages in a full Nasdaq day, and the seconds it took to rebuild every book on one core.

Whatever the answer turns out to be goes at the top of this file, with one chart, before any installation step.

## Sources

- Nasdaq TotalView-ITCH 5.0 specification (nasdaqtrader.com)

Every number that differs from a source will be listed in `PAPER.md` with the likely reason.

## Data

Nasdaq publishes full-day ITCH 5.0 sample files, free and without a login, at emi.nasdaq.com/ITCH. Each day is 3.5 to 18 GB. The directory states no licence, so no file from it is committed here. A script downloads one day.

## How it is tested

The maths is tested before any result is trusted.

- Unit tests for every message type
- Property tests: the book is never crossed, shares in equal shares out
- A golden test on the first million messages of one sample day

## Layout

```
src/               the order book, the parser, the replay tool
tests/             unit, property (proptest) and golden tests
benches/           throughput and latency benchmarks (criterion)
scripts/           fetch one sample day; reproduce the benchmark table
results/           committed benchmark tables and charts
PAPER.md           the specification and what was checked against it
```

## Roadmap

- [ ] Project skeleton, CI, and the reproduce command
- [ ] A price-time-priority limit order book: add, cancel, replace, execute
- [ ] A parser for Nasdaq TotalView-ITCH 5.0
- [ ] A replay tool that rebuilds the book for every symbol across one full trading day
- [ ] A benchmark: messages per second, and latency percentiles per message type
- [ ] A checker that the rebuilt best bid and offer never cross and that volume is conserved
- [ ] `PAPER.md` comparison with the sources
- [ ] Results and limits written up at the top of this file

## Limits

- It replays an exchange's book. It is not connected to anything and sends no orders.
- Benchmarks are from one desktop CPU. They say how this code compares with itself, not with a production engine.
- Hidden orders and auctions are handled as far as the public feed describes them, and no further.
- This is research code. It is not investment advice and it does not trade.

## The name

The agora was the marketplace: where buyers and sellers met and a price was made.

## Licence

MIT. See `LICENSE`.
