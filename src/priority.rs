//! Which executions skip the queue, and what they have in common.
//!
//! The replay counts `E` executions that hit an order at the best price that
//! was not the oldest order there. This module looks at each of those and
//! tests the explanations a reader would think of first: a minimum-quantity
//! incoming order skipping small resting orders, odd lots, replaced orders
//! whose queue place this project guessed wrong, self-match prevention
//! between attributed orders, halts, and the time of day.
//!
//! It also tests one explanation found by looking at this data, so it is
//! post hoc. Just after 09:30 the feed adds orders in bursts: many Add Order
//! messages for one stock with one timestamp. Here a *burst* is three or more
//! consecutive add messages with the same stock and timestamp, and a *burst
//! order* is an order added in one. The analysis reports how many
//! out-of-turn executions hit a burst order, and whether treating those
//! orders as older than they look is consistent with every later execution.
//!
//! The consistency test runs a second replay beside the first. In it, the
//! first time a burst order is executed out of turn it is moved to the front
//! of its queue. If burst orders really do rank ahead of what they skipped,
//! nothing should be out of turn after that move: no order should need
//! moving twice, and no other kind of order should ever skip the queue.

use std::fmt::Write as _;

use crate::index::RefMap;
use crate::itch::{Body, Message};
use crate::replay::{ReplacePolicy, Replay, OPEN_NS};
use crate::report::clock;

/// Adds in a row, same stock and timestamp, that make a burst.
pub const BURST_MIN: u32 = 3;

const HALF_HOUR_NS: u64 = 1_800_000_000_000;
const NO_BURST: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct Meta {
    /// When the add or replace message arrived.
    ts: u64,
    /// Index into `bursts`, or `NO_BURST`.
    burst: u32,
    /// Attribution, or zeros.
    mpid: [u8; 4],
    /// `A`, `F` or `U`.
    origin: u8,
    /// For `U`: the replacement kept the price and did not grow.
    kept_price_not_larger: bool,
    /// Already executed out of turn once (first replay).
    seen_out_of_turn: bool,
    /// Already moved to the front (second replay).
    promoted: bool,
}

/// Counts from one pass over a file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PriorityReport {
    /// `E` executions against a known order.
    pub executions: u64,
    /// `E` executions at the best price that did not take the oldest order.
    pub out_of_turn: u64,
    /// Distinct orders executed out of turn.
    pub out_of_turn_orders: u64,
    /// Stocks with at least one out-of-turn execution.
    pub out_of_turn_stocks: u64,

    /// Bursts in the file.
    pub bursts: u64,
    /// Orders added in bursts.
    pub burst_orders: u64,
    /// Stocks with at least one burst.
    pub burst_stocks: u64,
    /// Longest burst, in messages.
    pub longest_burst: u32,
    /// Timestamp of the first burst.
    pub first_burst_ts: u64,
    /// Timestamp of the last burst.
    pub last_burst_ts: u64,
    /// Bursts outside 09:30:00 to 09:31:00.
    pub bursts_outside_first_minute: u64,

    /// `E` executions against burst orders.
    pub exec_burst: u64,
    /// Of those, out of turn.
    pub exec_burst_out_of_turn: u64,
    /// `E` executions against every other order.
    pub exec_other: u64,
    /// Of those, out of turn.
    pub exec_other_out_of_turn: u64,
    /// Shortest burst that an out-of-turn order came from.
    pub shortest_burst_out_of_turn: u32,
    /// Out-of-turn executions whose order was added at or after its stock's
    /// opening cross message.
    pub after_own_opening_cross: u64,

    /// Out-of-turn executions by how the executed order was added.
    pub executed_from_a: u64,
    /// Added by `F`.
    pub executed_from_f: u64,
    /// Added by `U`.
    pub executed_from_u: u64,
    /// Out-of-turn executions that filled the order completely.
    pub fully_filled: u64,
    /// Out-of-turn executions while the stock's trading state was not `T`.
    pub not_trading: u64,

    /// Orders ahead of the executed order, summed over out-of-turn
    /// executions.
    pub skipped: u64,
    /// Skipped orders that were burst orders.
    pub skipped_burst: u64,
    /// Skipped orders that came from a replace.
    pub skipped_from_replace: u64,
    /// Skipped orders from a replace that kept its price and did not grow:
    /// the ones the other reading of Order Replace would place differently.
    pub skipped_from_keepable_replace: u64,
    /// Skipped orders with the same attribution as the executed order.
    pub skipped_same_mpid: u64,
    /// Out-of-turn executions where every skipped order was smaller than the
    /// executed shares: what a minimum-quantity incoming order would produce.
    pub all_skipped_smaller_than_fill: u64,
    /// Out-of-turn executions where every skipped order was under 100 shares.
    pub all_skipped_under_100: u64,

    /// What happened next to each skipped order: executed in the same
    /// timestamp.
    pub skipped_then_executed_same_ts: u64,
    /// Executed later.
    pub skipped_then_executed_later: u64,
    /// Deleted.
    pub skipped_then_deleted: u64,
    /// Partly cancelled.
    pub skipped_then_cancelled: u64,
    /// Replaced.
    pub skipped_then_replaced: u64,

    /// Out-of-turn executions by listing market: `Q`, `G`, `S`, other.
    pub by_market: [u64; 4],
    /// Executions and out-of-turn executions per half-hour of the day.
    pub by_half_hour: Vec<(u64, u64)>,

    /// Second replay: burst orders moved to the front on their first
    /// out-of-turn execution.
    pub promotions: u64,
    /// Second replay: out-of-turn executions of an order already moved.
    pub out_of_turn_after_promotion: u64,
    /// Second replay: out-of-turn executions of an order that is not a
    /// burst order.
    pub out_of_turn_not_burst_after_promotion: u64,
}

#[derive(Clone, Copy)]
struct Burst {
    len: u32,
}

/// Follows a message stream and fills a [`PriorityReport`].
pub struct Priority {
    policy: ReplacePolicy,
    plain: Replay,
    promoted: Replay,
    meta: RefMap<Meta>,
    /// Skipped orders waiting for their next message: when they were skipped.
    watch: RefMap<Vec<u64>>,
    bursts: Vec<Burst>,
    states: Vec<u8>,
    market: Vec<u8>,
    open_cross: Vec<u64>,
    burst_stock: Vec<bool>,
    anomaly_stock: Vec<bool>,
    // The run of add messages in progress.
    run_ts: u64,
    run_locate: u16,
    run_len: u32,
    run_burst: u32,
    run_refs: [u64; 2],
    last_was_add: bool,
    report: PriorityReport,
}

impl Default for Priority {
    fn default() -> Self {
        Self::new()
    }
}

impl Priority {
    /// A fresh analysis, with replaced orders going to the back of the queue.
    pub fn new() -> Self {
        Self::with_policy(ReplacePolicy::BackOfQueue)
    }

    /// A fresh analysis under a given reading of Order Replace.
    pub fn with_policy(policy: ReplacePolicy) -> Self {
        Priority {
            policy,
            plain: Replay::new(false).with_policy(policy),
            promoted: Replay::new(false).with_policy(policy),
            meta: RefMap::default(),
            watch: RefMap::default(),
            bursts: Vec::new(),
            states: vec![0; 1 << 16],
            market: vec![0; 1 << 16],
            open_cross: vec![0; 1 << 16],
            burst_stock: vec![false; 1 << 16],
            anomaly_stock: vec![false; 1 << 16],
            run_ts: u64::MAX,
            run_locate: 0,
            run_len: 0,
            run_burst: NO_BURST,
            run_refs: [0; 2],
            last_was_add: false,
            report: PriorityReport {
                shortest_burst_out_of_turn: u32::MAX,
                ..PriorityReport::default()
            },
        }
    }

    fn is_burst(&self, m: &Meta) -> bool {
        m.burst != NO_BURST
    }

    /// Note an add message and work out whether it extends a burst.
    fn on_add(&mut self, order_ref: u64, ts: u64, locate: u16, mpid: Option<[u8; 4]>) {
        if self.last_was_add && ts == self.run_ts && locate == self.run_locate {
            self.run_len += 1;
        } else {
            self.run_ts = ts;
            self.run_locate = locate;
            self.run_len = 1;
            self.run_burst = NO_BURST;
        }
        if self.run_len <= 2 {
            self.run_refs[(self.run_len - 1) as usize] = order_ref;
        }
        if self.run_len == BURST_MIN {
            // The run has just become a burst: give it a number and mark the
            // orders that opened it.
            self.run_burst = self.bursts.len() as u32;
            self.bursts.push(Burst { len: BURST_MIN - 1 });
            for r in self.run_refs {
                if let Some(meta) = self.meta.get_mut(&r) {
                    meta.burst = self.run_burst;
                }
            }
            let rep = &mut self.report;
            rep.bursts += 1;
            rep.burst_orders += u64::from(BURST_MIN - 1);
            if rep.bursts == 1 {
                rep.first_burst_ts = ts;
            }
            rep.last_burst_ts = ts;
            if !(OPEN_NS..OPEN_NS + 60_000_000_000).contains(&ts) {
                rep.bursts_outside_first_minute += 1;
            }
            self.burst_stock[usize::from(locate)] = true;
        }
        if self.run_burst != NO_BURST {
            let b = &mut self.bursts[self.run_burst as usize];
            b.len += 1;
            self.report.burst_orders += 1;
            self.report.longest_burst = self.report.longest_burst.max(b.len);
        }
        self.meta.insert(
            order_ref,
            Meta {
                ts,
                burst: self.run_burst,
                mpid: mpid.unwrap_or([0; 4]),
                origin: if mpid.is_some() { b'F' } else { b'A' },
                kept_price_not_larger: false,
                seen_out_of_turn: false,
                promoted: false,
            },
        );
    }

    /// A skipped order has its next message: record what it was.
    fn on_touch(&mut self, order_ref: u64, ts: u64, kind: u8) {
        let Some(skips) = self.watch.remove(&order_ref) else {
            return;
        };
        let rep = &mut self.report;
        for skipped_at in skips {
            match kind {
                b'E' | b'C' if skipped_at == ts => rep.skipped_then_executed_same_ts += 1,
                b'E' | b'C' => rep.skipped_then_executed_later += 1,
                b'D' => rep.skipped_then_deleted += 1,
                b'X' => rep.skipped_then_cancelled += 1,
                _ => rep.skipped_then_replaced += 1,
            }
        }
    }

    /// Look at an `E` execution before it is applied.
    fn on_execute(&mut self, order_ref: u64, shares: u32, ts: u64) {
        // First replay: the book exactly as the feed's message order gives it.
        if let Some(resting) = self.plain.locate_order(order_ref) {
            let locate = usize::from(resting.locate);
            let meta = self.meta.get(&order_ref).copied();
            let burst = meta.is_some_and(|m| self.is_burst(&m));
            let book = self
                .plain
                .book(resting.locate)
                .expect("the order has a book");
            let out_of_turn = book.at_best(resting.handle) && !book.at_head(resting.handle);
            let half_hour = (ts / HALF_HOUR_NS) as usize;
            let rep = &mut self.report;
            if rep.by_half_hour.len() <= half_hour {
                rep.by_half_hour.resize(half_hour + 1, (0, 0));
            }
            rep.executions += 1;
            rep.by_half_hour[half_hour].0 += 1;
            if burst {
                rep.exec_burst += 1;
            } else {
                rep.exec_other += 1;
            }
            if out_of_turn {
                rep.out_of_turn += 1;
                rep.by_half_hour[half_hour].1 += 1;
                if burst {
                    rep.exec_burst_out_of_turn += 1;
                } else {
                    rep.exec_other_out_of_turn += 1;
                }
                if !self.anomaly_stock[locate] {
                    self.anomaly_stock[locate] = true;
                    rep.out_of_turn_stocks += 1;
                }
                if self.states[locate] != b'T' {
                    rep.not_trading += 1;
                }
                match self.market[locate] {
                    b'Q' => rep.by_market[0] += 1,
                    b'G' => rep.by_market[1] += 1,
                    b'S' => rep.by_market[2] += 1,
                    _ => rep.by_market[3] += 1,
                }
                let order = book.get(resting.handle).expect("the order rests");
                if shares >= order.qty {
                    rep.fully_filled += 1;
                }
                let mut executed_mpid = [0; 4];
                if let Some(m) = meta {
                    match m.origin {
                        b'A' => rep.executed_from_a += 1,
                        b'F' => rep.executed_from_f += 1,
                        _ => rep.executed_from_u += 1,
                    }
                    executed_mpid = m.mpid;
                    if m.burst != NO_BURST {
                        let len = self.bursts[m.burst as usize].len;
                        rep.shortest_burst_out_of_turn = rep.shortest_burst_out_of_turn.min(len);
                    }
                    let cross = self.open_cross[locate];
                    if cross != 0 && m.ts >= cross {
                        rep.after_own_opening_cross += 1;
                    }
                    if !m.seen_out_of_turn {
                        rep.out_of_turn_orders += 1;
                    }
                }
                let (mut all_smaller, mut all_under_100) = (true, true);
                for ahead in book.queue(order.side, order.price) {
                    if ahead.id == order_ref {
                        break;
                    }
                    rep.skipped += 1;
                    all_smaller &= ahead.qty < shares;
                    all_under_100 &= ahead.qty < 100;
                    if let Some(a) = self.meta.get(&ahead.id) {
                        if a.burst != NO_BURST {
                            rep.skipped_burst += 1;
                        }
                        if a.origin == b'U' {
                            rep.skipped_from_replace += 1;
                            if a.kept_price_not_larger {
                                rep.skipped_from_keepable_replace += 1;
                            }
                        }
                        if executed_mpid != [0; 4] && a.mpid == executed_mpid {
                            rep.skipped_same_mpid += 1;
                        }
                    }
                    self.watch.entry(ahead.id).or_default().push(ts);
                }
                if all_smaller {
                    rep.all_skipped_smaller_than_fill += 1;
                }
                if all_under_100 {
                    rep.all_skipped_under_100 += 1;
                }
                if let Some(m) = self.meta.get_mut(&order_ref) {
                    m.seen_out_of_turn = true;
                }
            }
        }

        // Second replay: the same check, then move a burst order to the
        // front the first time it is executed out of turn.
        if let Some(resting) = self.promoted.locate_order(order_ref) {
            let book = self
                .promoted
                .book(resting.locate)
                .expect("the order has a book");
            if book.at_best(resting.handle) && !book.at_head(resting.handle) {
                match self.meta.get_mut(&order_ref) {
                    Some(m) if m.burst != NO_BURST && !m.promoted => {
                        m.promoted = true;
                        self.report.promotions += 1;
                        self.promoted.promote(order_ref);
                    }
                    Some(m) if m.burst != NO_BURST => {
                        self.report.out_of_turn_after_promotion += 1;
                    }
                    _ => self.report.out_of_turn_not_burst_after_promotion += 1,
                }
            }
        }
    }

    /// Apply one message to both replays.
    pub fn apply(&mut self, m: &Message) {
        let ts = m.header.timestamp;
        let locate = m.header.locate;
        let mut is_add = false;
        let mut maybe_dead = None;
        match m.body {
            Body::AddOrder {
                order_ref,
                shares,
                mpid,
                ..
            } => {
                // The replay ignores a zero-share add and a duplicate
                // reference; so does this.
                if shares > 0 && self.plain.locate_order(order_ref).is_none() {
                    is_add = true;
                    self.on_add(order_ref, ts, locate, mpid);
                }
            }
            Body::OrderExecuted {
                order_ref, shares, ..
            } => {
                self.on_touch(order_ref, ts, b'E');
                self.on_execute(order_ref, shares, ts);
                maybe_dead = Some(order_ref);
            }
            Body::OrderExecutedWithPrice { order_ref, .. } => {
                self.on_touch(order_ref, ts, b'C');
                maybe_dead = Some(order_ref);
            }
            Body::OrderCancel { order_ref, .. } => {
                self.on_touch(order_ref, ts, b'X');
                maybe_dead = Some(order_ref);
            }
            Body::OrderDelete { order_ref } => {
                self.on_touch(order_ref, ts, b'D');
                self.meta.remove(&order_ref);
            }
            Body::OrderReplace {
                old_ref,
                new_ref,
                shares,
                price,
            } => {
                self.on_touch(old_ref, ts, b'U');
                let old = self.plain.locate_order(old_ref).and_then(|r| {
                    self.plain
                        .book(r.locate)
                        .and_then(|book| book.get(r.handle))
                });
                let old_meta = self.meta.remove(&old_ref);
                if let (Some(old), Some(old_meta)) = (old, old_meta) {
                    if shares > 0 && self.plain.locate_order(new_ref).is_none() {
                        let kept = price == old.price && shares <= old.qty;
                        // Under the other reading such an order keeps its
                        // place, so it is still the burst order it was.
                        let same_place =
                            kept && self.policy == ReplacePolicy::KeepIfSamePriceAndNotLarger;
                        self.meta.insert(
                            new_ref,
                            Meta {
                                ts,
                                burst: if same_place { old_meta.burst } else { NO_BURST },
                                mpid: old_meta.mpid,
                                origin: b'U',
                                kept_price_not_larger: kept,
                                seen_out_of_turn: same_place && old_meta.seen_out_of_turn,
                                promoted: same_place && old_meta.promoted,
                            },
                        );
                    }
                }
            }
            Body::CrossTrade { cross_type, .. } => {
                if cross_type == b'O' {
                    self.open_cross[usize::from(locate)] = ts;
                }
            }
            Body::StockDirectory {
                market_category, ..
            } => {
                self.market[usize::from(locate)] = market_category;
            }
            Body::TradingAction { state, .. } => {
                self.states[usize::from(locate)] = state;
            }
            _ => {}
        }
        self.last_was_add = is_add;
        self.plain.apply(m);
        self.promoted.apply(m);
        if let Some(r) = maybe_dead {
            if self.plain.locate_order(r).is_none() {
                self.meta.remove(&r);
            }
        }
    }

    /// The counts so far.
    pub fn report(&self) -> PriorityReport {
        let mut rep = self.report.clone();
        rep.burst_stocks = self.burst_stock.iter().filter(|b| **b).count() as u64;
        if rep.shortest_burst_out_of_turn == u32::MAX {
            rep.shortest_burst_out_of_turn = 0;
        }
        rep
    }
}

impl PriorityReport {
    /// The report as `measure,value` rows.
    pub fn to_csv(&self) -> String {
        let mut out = String::from("measure,value\n");
        let mut row = |k: &str, v: u64| {
            let _ = writeln!(out, "{k},{v}");
        };
        row("executions_E", self.executions);
        row("out_of_turn", self.out_of_turn);
        row("out_of_turn_distinct_orders", self.out_of_turn_orders);
        row("out_of_turn_stocks", self.out_of_turn_stocks);
        row("bursts", self.bursts);
        row("burst_orders", self.burst_orders);
        row("burst_stocks", self.burst_stocks);
        row("longest_burst", u64::from(self.longest_burst));
        row(
            "bursts_outside_0930_to_0931",
            self.bursts_outside_first_minute,
        );
        row("executions_of_burst_orders", self.exec_burst);
        row(
            "executions_of_burst_orders_out_of_turn",
            self.exec_burst_out_of_turn,
        );
        row("executions_of_other_orders", self.exec_other);
        row(
            "executions_of_other_orders_out_of_turn",
            self.exec_other_out_of_turn,
        );
        row(
            "out_of_turn_shortest_burst_of_executed_order",
            u64::from(self.shortest_burst_out_of_turn),
        );
        row(
            "out_of_turn_order_added_after_its_opening_cross",
            self.after_own_opening_cross,
        );
        row("out_of_turn_order_added_by_A", self.executed_from_a);
        row("out_of_turn_order_added_by_F", self.executed_from_f);
        row("out_of_turn_order_added_by_U", self.executed_from_u);
        row("out_of_turn_order_fully_filled", self.fully_filled);
        row("out_of_turn_stock_not_trading", self.not_trading);
        row("out_of_turn_nasdaq_global_select", self.by_market[0]);
        row("out_of_turn_nasdaq_global_market", self.by_market[1]);
        row("out_of_turn_nasdaq_capital_market", self.by_market[2]);
        row("out_of_turn_listed_elsewhere", self.by_market[3]);
        row("skipped_orders", self.skipped);
        row("skipped_orders_from_bursts", self.skipped_burst);
        row("skipped_orders_from_replace", self.skipped_from_replace);
        row(
            "skipped_orders_from_replace_same_price_not_larger",
            self.skipped_from_keepable_replace,
        );
        row(
            "skipped_orders_same_mpid_as_executed",
            self.skipped_same_mpid,
        );
        row(
            "out_of_turn_all_skipped_smaller_than_fill",
            self.all_skipped_smaller_than_fill,
        );
        row(
            "out_of_turn_all_skipped_under_100_shares",
            self.all_skipped_under_100,
        );
        row(
            "skipped_next_executed_same_timestamp",
            self.skipped_then_executed_same_ts,
        );
        row(
            "skipped_next_executed_later",
            self.skipped_then_executed_later,
        );
        row("skipped_next_deleted", self.skipped_then_deleted);
        row("skipped_next_partly_cancelled", self.skipped_then_cancelled);
        row("skipped_next_replaced", self.skipped_then_replaced);
        row("promote_test_orders_moved_to_front", self.promotions);
        row(
            "promote_test_out_of_turn_again_after_move",
            self.out_of_turn_after_promotion,
        );
        row(
            "promote_test_out_of_turn_not_a_burst_order",
            self.out_of_turn_not_burst_after_promotion,
        );
        let _ = writeln!(out, "first_burst,{}", clock(self.first_burst_ts));
        let _ = writeln!(out, "last_burst,{}", clock(self.last_burst_ts));
        out
    }

    /// Executions and out-of-turn executions per half-hour, as CSV.
    pub fn by_time_csv(&self) -> String {
        let mut out = String::from("half_hour_from,executions_E,out_of_turn\n");
        for (i, (all, odd)) in self.by_half_hour.iter().enumerate() {
            if *all == 0 {
                continue;
            }
            let minutes = i * 30;
            let _ = writeln!(out, "{:02}:{:02},{all},{odd}", minutes / 60, minutes % 60);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::Side;
    use crate::itch::{stock, Header};

    fn msg(ts: u64, body: Body) -> Message {
        Message {
            header: Header {
                locate: 1,
                tracking: 0,
                timestamp: ts,
            },
            body,
        }
    }

    fn add(ts: u64, order_ref: u64, shares: u32) -> Message {
        msg(
            ts,
            Body::AddOrder {
                order_ref,
                side: Side::Sell,
                shares,
                stock: stock("TEST"),
                price: 100,
                mpid: None,
            },
        )
    }

    fn exec(ts: u64, order_ref: u64, shares: u32) -> Message {
        msg(
            ts,
            Body::OrderExecuted {
                order_ref,
                shares,
                match_number: 0,
            },
        )
    }

    fn run(stream: &[Message]) -> PriorityReport {
        let mut p = Priority::new();
        for m in stream {
            p.apply(m);
        }
        p.report()
    }

    #[test]
    fn a_burst_order_that_skips_the_queue_is_explained() {
        let t = OPEN_NS + 5;
        let rep = run(&[
            add(10, 1, 50),
            add(20, 2, 500),
            // Three adds at one timestamp: a burst.
            add(t, 3, 100),
            add(t, 4, 100),
            add(t, 5, 100),
            // Order 3 is executed with orders 1 and 2 ahead of it.
            exec(t + 10, 3, 100),
            // Order 1 trades in the same timestamp; order 2 is deleted later.
            exec(t + 10, 1, 50),
            msg(t + 99, Body::OrderDelete { order_ref: 2 }),
            // Order 4 is now the oldest: in turn.
            exec(t + 100, 4, 100),
        ]);
        assert_eq!(rep.executions, 3);
        assert_eq!(rep.out_of_turn, 1);
        assert_eq!(rep.out_of_turn_orders, 1);
        assert_eq!((rep.bursts, rep.burst_orders, rep.longest_burst), (1, 3, 3));
        assert_eq!(rep.bursts_outside_first_minute, 0);
        assert_eq!((rep.exec_burst, rep.exec_burst_out_of_turn), (2, 1));
        assert_eq!((rep.exec_other, rep.exec_other_out_of_turn), (1, 0));
        assert_eq!(rep.shortest_burst_out_of_turn, 3);
        assert_eq!(rep.skipped, 2);
        assert_eq!(rep.skipped_burst, 0);
        // Order 2 (500 shares) is not smaller than the 100 executed.
        assert_eq!(rep.all_skipped_smaller_than_fill, 0);
        assert_eq!(rep.skipped_then_executed_same_ts, 1);
        assert_eq!(rep.skipped_then_deleted, 1);
        assert_eq!(rep.fully_filled, 1);
        assert_eq!(rep.promotions, 1);
        assert_eq!(rep.out_of_turn_after_promotion, 0);
        assert_eq!(rep.out_of_turn_not_burst_after_promotion, 0);
        let csv = rep.to_csv();
        assert!(csv.contains("out_of_turn,1\n"));
        assert!(csv.contains("first_burst,09:30:00.000000005\n"));
        assert!(rep.by_time_csv().contains("09:30,3,1\n"));
    }

    #[test]
    fn an_ordinary_order_that_skips_the_queue_is_not_explained() {
        let rep = run(&[
            add(10, 1, 50),
            add(20, 2, 80),
            // Order 2 trades ahead of order 1, and neither is a burst order.
            exec(30, 2, 80),
        ]);
        assert_eq!(rep.out_of_turn, 1);
        assert_eq!(rep.bursts, 0);
        assert_eq!((rep.exec_other, rep.exec_other_out_of_turn), (1, 1));
        assert_eq!(rep.all_skipped_smaller_than_fill, 1);
        assert_eq!(rep.all_skipped_under_100, 1);
        assert_eq!(rep.promotions, 0);
        assert_eq!(rep.out_of_turn_not_burst_after_promotion, 1);
    }

    #[test]
    fn a_moved_order_that_is_skipped_in_turn_fails_the_promote_test() {
        let t = OPEN_NS;
        let rep = run(&[
            add(10, 1, 50),
            add(t, 3, 100),
            add(t, 4, 100),
            add(t, 5, 100),
            // Order 4 jumps order 1 and order 3, and is moved to the front.
            exec(t + 1, 4, 10),
            // Then order 5 jumps the queue too and is moved ahead of order 4.
            exec(t + 2, 5, 10),
            // Order 4 trading now means it was not behind order 5 after all.
            exec(t + 3, 4, 10),
        ]);
        assert_eq!(rep.out_of_turn, 3);
        assert_eq!(rep.out_of_turn_orders, 2);
        assert_eq!(rep.promotions, 2);
        assert_eq!(rep.out_of_turn_after_promotion, 1);
        // A burst order ahead of the executed one counts as a skipped burst
        // order.
        assert!(rep.skipped_burst >= 1);
    }

    #[test]
    fn two_adds_at_one_timestamp_are_not_a_burst() {
        let rep = run(&[add(5, 1, 10), add(5, 2, 10), exec(6, 1, 10)]);
        assert_eq!(rep.bursts, 0);
        assert_eq!(rep.out_of_turn, 0);
        assert_eq!(rep.exec_other, 1);
    }
}
