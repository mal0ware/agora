//! Agora: a price-time-priority limit order book and a Nasdaq
//! TotalView-ITCH 5.0 replay.
//!
//! - [`book`]: one instrument's book, with first-in-first-out queues per price.
//! - [`engine`]: a matching engine on top of the book.
//! - [`index`]: the order reference index, a page table keyed by reference.
//! - [`itch`]: the ITCH 5.0 message formats and a reader for framed files.
//! - [`replay`]: rebuilds every book from a message stream and checks it.
//! - [`synth`]: a seeded generator of valid ITCH streams for tests.
//! - [`hist`]: a cycle clock and a histogram for latency percentiles.
//! - [`pin`]: pins the benchmark thread to one CPU.
//! - [`priority`]: which executions skip the queue, and what they have in common.
//! - [`report`]: the tables and the chart written by the reproduce command.

#![warn(missing_docs)]

pub mod book;
pub mod engine;
pub mod hist;
pub mod index;
pub mod itch;
mod mem;
pub mod pin;
pub mod priority;
pub mod replay;
pub mod report;
pub mod synth;
