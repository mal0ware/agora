//! Rebuild every book from a stream of ITCH messages, and check the result.
//!
//! The exchange has already matched the orders, so replay only applies what
//! the feed says: add (`A`, `F`), execute (`E`, `C`), cancel (`X`), delete
//! (`D`) and replace (`U`). Every other message leaves the books alone.
//!
//! With checks on, the replay also records after every book update whether
//! that book is crossed, whether each execution took the oldest order at the
//! best price, and at the end audits every book and balances the share ledger.

use crate::book::{digest_books, Book, Handle, Side};
use crate::index::{OrderIndex, RefIndex};
use crate::itch::{Body, Message, Stock};

/// 09:30:00 in nanoseconds since midnight.
pub const OPEN_NS: u64 = 34_200_000_000_000;
/// 16:00:00 in nanoseconds since midnight.
pub const CLOSE_NS: u64 = 57_600_000_000_000;

/// How many crossed episodes to keep as examples.
const EXAMPLES: usize = 40;

/// Where one order rests: which stock's book, and its handle there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resting {
    /// Handle in that stock's book.
    pub handle: Handle,
    /// Stock locate.
    pub locate: u16,
}

/// Counters kept while replaying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    /// Messages applied.
    pub messages: u64,
    /// Messages by type byte.
    pub counts: [u64; 256],
    /// Orders added by `A`, `F` and `U`.
    pub orders_added: u64,
    /// Shares added by `A`, `F` and `U`.
    pub shares_added: u64,
    /// Shares executed by `E` and `C`.
    pub shares_executed: u64,
    /// Shares removed by `X`.
    pub shares_cancelled: u64,
    /// Shares removed by `D`.
    pub shares_deleted: u64,
    /// Shares the old order still had when `U` replaced it.
    pub shares_replaced: u64,
    /// Messages naming an order reference that is not on any book.
    pub unknown_ref: u64,
    /// Adds or replaces whose new reference is already on a book.
    pub duplicate_ref: u64,
    /// Executions or cancels for more shares than the order has left.
    pub over_reduce: u64,
    /// Order messages whose stock locate differs from the order's.
    pub locate_mismatch: u64,
    /// Adds or replaces with zero shares.
    pub zero_share_add: u64,
    /// Times a book refused a request the replay thought was valid. Always
    /// zero unless the order index and a book have fallen out of step.
    pub book_faults: u64,
    /// Largest number of orders resting at once across all books.
    pub peak_live_orders: u64,
    /// Printable executed shares: `E`, printable `C`, and `P`.
    pub printed_shares: u64,
    /// Shares reported by cross trades (`Q`).
    pub cross_shares: u64,
    /// Non-printable `C` shares (included in a later cross print).
    pub non_printable_shares: u64,
    /// Timestamp of the first message.
    pub first_ts: u64,
    /// Timestamp of the last message.
    pub last_ts: u64,
    /// Messages whose timestamp is lower than the one before.
    pub time_reversals: u64,

    // The counters below are filled only when checks are on.
    /// Messages that changed a book.
    pub book_updates: u64,
    /// Book updates after which that book was crossed.
    pub crossed_updates: u64,
    /// Book updates after which that book was locked.
    pub locked_updates: u64,
    /// Times a book went from not crossed to crossed.
    pub crossed_episodes: u64,
    /// Crossed episodes that began while the stock's state was `T`.
    pub crossed_episodes_trading: u64,
    /// Crossed episodes that began before 09:30, from 09:30 to 16:00, after.
    pub crossed_episodes_by_session: [u64; 3],
    /// Total time books spent crossed, summed over books.
    pub crossed_ns: u64,
    /// `E` executions.
    pub exec_e: u64,
    /// `E` executions against an order at the best price on its side.
    pub exec_e_at_best: u64,
    /// `E` executions against the oldest order at the best price.
    pub exec_e_at_front: u64,
    /// `E` executions that did not take the oldest order at the best price,
    /// split into before 09:30, 09:30 to 16:00, and after.
    pub exec_e_not_front_by_session: [u64; 3],
    /// `E` executions that did not take the oldest order at the best price
    /// while the stock's trading state was not `T`.
    pub exec_e_not_front_not_trading: u64,
    /// `C` executions.
    pub exec_c: u64,
    /// `C` executions against an order at the best price on its side.
    pub exec_c_at_best: u64,
    /// `C` executions against the oldest order at the best price.
    pub exec_c_at_front: u64,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            messages: 0,
            counts: [0; 256],
            orders_added: 0,
            shares_added: 0,
            shares_executed: 0,
            shares_cancelled: 0,
            shares_deleted: 0,
            shares_replaced: 0,
            unknown_ref: 0,
            duplicate_ref: 0,
            over_reduce: 0,
            locate_mismatch: 0,
            zero_share_add: 0,
            book_faults: 0,
            peak_live_orders: 0,
            printed_shares: 0,
            cross_shares: 0,
            non_printable_shares: 0,
            first_ts: 0,
            last_ts: 0,
            time_reversals: 0,
            book_updates: 0,
            crossed_updates: 0,
            locked_updates: 0,
            crossed_episodes: 0,
            crossed_episodes_trading: 0,
            crossed_episodes_by_session: [0; 3],
            crossed_ns: 0,
            exec_e: 0,
            exec_e_at_best: 0,
            exec_e_at_front: 0,
            exec_e_not_front_by_session: [0; 3],
            exec_e_not_front_not_trading: 0,
            exec_c: 0,
            exec_c_at_best: 0,
            exec_c_at_front: 0,
        }
    }
}

/// One crossed episode, kept as an example.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossExample {
    /// When the book became crossed.
    pub timestamp: u64,
    /// Stock locate.
    pub locate: u16,
    /// Best bid at that moment.
    pub bid: u32,
    /// Best ask at that moment.
    pub ask: u32,
    /// Trading state of the stock (`T`, `H`, `P`, `Q`, or 0 if never set).
    pub state: u8,
    /// Type of the message that crossed the book.
    pub msg_type: u8,
}

/// Per-stock crossed totals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CrossTotals {
    /// Episodes.
    pub episodes: u64,
    /// Book updates that left the book crossed.
    pub updates: u64,
    /// Time spent crossed.
    pub ns: u64,
}

#[derive(Debug, Clone, Copy)]
struct CrossState {
    /// Start of the open episode, or `u64::MAX` when not crossed.
    since: u64,
    totals: CrossTotals,
}

impl Default for CrossState {
    fn default() -> Self {
        CrossState {
            since: u64::MAX,
            totals: CrossTotals::default(),
        }
    }
}

/// What the end-of-run audit found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Audit {
    /// Books with at least one resting order.
    pub books_with_orders: u64,
    /// Orders resting at the end, counted by walking the books.
    pub resting_orders: u64,
    /// Shares resting at the end, from the level aggregates.
    pub resting_shares: u64,
    /// Entries left in the order reference map.
    pub refs_live: u64,
    /// First structural error per failing book, if any.
    pub structure_errors: Vec<String>,
    /// Books crossed at the end.
    pub crossed_at_end: u64,
    /// Shares added minus shares executed, cancelled, deleted, replaced and
    /// resting. Zero when volume is conserved.
    pub ledger_gap: i128,
    /// Digest of every book.
    pub digest: u64,
}

impl Audit {
    /// True when the structures are sound, the ledger balances, and the order
    /// reference map agrees with the books.
    pub fn conserved(&self) -> bool {
        self.structure_errors.is_empty()
            && self.ledger_gap == 0
            && self.refs_live == self.resting_orders
    }
}

/// Where a replaced order goes in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplacePolicy {
    /// Every replacement joins the back of the queue at its price. This is
    /// the default and the reading used for every published number.
    #[default]
    BackOfQueue,
    /// A replacement at the same price for no more shares keeps its place.
    /// An alternative reading, kept only to measure it against the data.
    KeepIfSamePriceAndNotLarger,
}

fn session(ts: u64) -> usize {
    if ts < OPEN_NS {
        0
    } else if ts < CLOSE_NS {
        1
    } else {
        2
    }
}

/// Replays ITCH messages into one book per stock locate.
///
/// `I` is the order reference index. Leave it alone to get the page table;
/// [`Replay::with_index`] picks another, which only the benchmark does.
pub struct Replay<I = RefIndex<Resting>> {
    policy: ReplacePolicy,
    books: Vec<Book>,
    symbols: Vec<Stock>,
    states: Vec<u8>,
    cross: Vec<CrossState>,
    refs: I,
    check: bool,
    /// Counters.
    pub stats: Stats,
    /// The first crossed episodes seen, as examples.
    pub examples: Vec<CrossExample>,
}

impl Replay {
    /// A replay with no books. `check` turns on the per-update invariant
    /// checks; without it only the books and the cheap counters are kept.
    pub fn new(check: bool) -> Self {
        Self::with_index(check)
    }
}

impl<I: OrderIndex<Resting>> Replay<I> {
    /// A replay with no books, on the order reference index `I`.
    pub fn with_index(check: bool) -> Self {
        Replay {
            policy: ReplacePolicy::BackOfQueue,
            books: Vec::new(),
            symbols: Vec::new(),
            states: Vec::new(),
            cross: Vec::new(),
            refs: I::default(),
            check,
            stats: Stats::default(),
            examples: Vec::new(),
        }
    }

    /// Use a different rule for where replaced orders go.
    pub fn with_policy(mut self, policy: ReplacePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Apply a replace that keeps the order's place: same price, no more
    /// shares. Returns false when the replace does not qualify.
    fn replace_in_place(
        &mut self,
        m: &Message,
        old_ref: u64,
        new_ref: u64,
        shares: u32,
        price: u32,
    ) -> bool {
        let Some(resting) = self.refs.get(old_ref) else {
            return false;
        };
        let locate = usize::from(resting.locate);
        let Some(old) = self.books[locate].get(resting.handle) else {
            return false;
        };
        if price != old.price || shares == 0 || shares > old.qty || self.refs.contains(new_ref) {
            return false;
        }
        if locate != usize::from(m.header.locate) {
            self.stats.locate_mismatch += 1;
        }
        let book = &mut self.books[locate];
        if book.reduce(resting.handle, old.qty - shares).is_err()
            || book.set_id(resting.handle, new_ref).is_err()
        {
            self.stats.book_faults += 1;
        }
        self.refs.remove(old_ref);
        self.refs.insert(new_ref, resting);
        self.stats.shares_replaced += u64::from(old.qty);
        self.stats.shares_added += u64::from(shares);
        self.stats.orders_added += 1;
        if self.check {
            self.after_update(locate, m.header.timestamp, b'U');
        }
        true
    }

    #[inline]
    fn ensure(&mut self, locate: usize) {
        if locate >= self.books.len() {
            self.books.resize_with(locate + 1, Book::new);
            self.symbols.resize(locate + 1, [b' '; 8]);
            self.states.resize(locate + 1, 0);
            self.cross.resize(locate + 1, CrossState::default());
        }
    }

    /// The book for a stock locate.
    pub fn book(&self, locate: u16) -> Option<&Book> {
        self.books.get(usize::from(locate))
    }

    /// The symbol for a stock locate, from the stock directory.
    pub fn symbol(&self, locate: u16) -> Option<&Stock> {
        self.symbols.get(usize::from(locate))
    }

    /// The stock locate for a symbol.
    pub fn locate_of(&self, symbol: &Stock) -> Option<u16> {
        self.symbols
            .iter()
            .position(|s| s == symbol)
            .map(|i| i as u16)
    }

    /// Every book with its stock locate.
    pub fn books(&self) -> impl Iterator<Item = (u16, &Book)> {
        self.books.iter().enumerate().map(|(i, b)| (i as u16, b))
    }

    /// Where an order rests, if it does.
    pub fn locate_order(&self, order_ref: u64) -> Option<Resting> {
        self.refs.get(order_ref)
    }

    /// Move an order to the front of the queue at its price. Used only by
    /// the queue-priority analysis; a replay never does this by itself.
    pub fn promote(&mut self, order_ref: u64) -> bool {
        match self.refs.get(order_ref) {
            Some(r) => self.books[usize::from(r.locate)]
                .move_to_front(r.handle)
                .is_ok(),
            None => false,
        }
    }

    /// Orders resting across all books.
    pub fn live_orders(&self) -> usize {
        self.refs.len()
    }

    /// Per-stock crossed totals, for stocks that were ever crossed.
    pub fn cross_totals(&self) -> Vec<(u16, CrossTotals)> {
        self.cross
            .iter()
            .enumerate()
            .filter(|(_, c)| c.totals.episodes > 0)
            .map(|(i, c)| (i as u16, c.totals))
            .collect()
    }

    /// Digest of every book.
    pub fn digest(&self) -> u64 {
        digest_books(self.books.iter().enumerate().map(|(i, b)| (i as u64, b)))
    }

    #[inline]
    fn after_update(&mut self, locate: usize, ts: u64, msg_type: u8) {
        self.stats.book_updates += 1;
        let book = &self.books[locate];
        let cross = &mut self.cross[locate];
        if book.is_crossed() {
            self.stats.crossed_updates += 1;
            cross.totals.updates += 1;
            if cross.since == u64::MAX {
                cross.since = ts;
                cross.totals.episodes += 1;
                self.stats.crossed_episodes += 1;
                let state = self.states[locate];
                if state == b'T' {
                    self.stats.crossed_episodes_trading += 1;
                }
                self.stats.crossed_episodes_by_session[session(ts)] += 1;
                if self.examples.len() < EXAMPLES {
                    self.examples.push(CrossExample {
                        timestamp: ts,
                        locate: locate as u16,
                        bid: book.best_bid().unwrap_or(0),
                        ask: book.best_ask().unwrap_or(0),
                        state,
                        msg_type,
                    });
                }
            }
        } else {
            if cross.since != u64::MAX {
                let ns = ts.saturating_sub(cross.since);
                cross.totals.ns += ns;
                self.stats.crossed_ns += ns;
                cross.since = u64::MAX;
            }
            if book.is_locked() {
                self.stats.locked_updates += 1;
            }
        }
    }

    #[inline]
    fn add(&mut self, m: &Message, order_ref: u64, side: Side, shares: u32, price: u32) {
        let locate = usize::from(m.header.locate);
        self.ensure(locate);
        if shares == 0 {
            self.stats.zero_share_add += 1;
            return;
        }
        if self.refs.contains(order_ref) {
            self.stats.duplicate_ref += 1;
            return;
        }
        match self.books[locate].add(order_ref, side, price, shares) {
            Ok(handle) => {
                self.refs.insert(
                    order_ref,
                    Resting {
                        handle,
                        locate: locate as u16,
                    },
                );
            }
            Err(_) => {
                self.stats.book_faults += 1;
                return;
            }
        }
        self.stats.orders_added += 1;
        self.stats.shares_added += u64::from(shares);
        let live = self.refs.len() as u64;
        if live > self.stats.peak_live_orders {
            self.stats.peak_live_orders = live;
        }
        if self.check {
            self.after_update(locate, m.header.timestamp, m.type_byte());
        }
    }

    /// Reduce an order. `shares` of `None` removes whatever is left. Returns
    /// the shares actually removed and the order's side, or `None` if the
    /// reference is unknown.
    #[inline]
    fn reduce(&mut self, m: &Message, order_ref: u64, shares: Option<u32>) -> Option<(u32, Side)> {
        let Some(resting) = self.refs.get(order_ref) else {
            self.stats.unknown_ref += 1;
            return None;
        };
        let locate = usize::from(resting.locate);
        let slot = resting.handle;
        if locate != usize::from(m.header.locate) {
            self.stats.locate_mismatch += 1;
        }
        let book = &mut self.books[locate];
        let Some(order) = book.get(slot) else {
            self.stats.book_faults += 1;
            self.refs.remove(order_ref);
            return None;
        };
        if self.check {
            match m.body {
                Body::OrderExecuted { .. } => {
                    self.stats.exec_e += 1;
                    let at_best = book.at_best(slot);
                    if at_best {
                        self.stats.exec_e_at_best += 1;
                    }
                    if at_best && book.at_head(slot) {
                        self.stats.exec_e_at_front += 1;
                    } else {
                        self.stats.exec_e_not_front_by_session[session(m.header.timestamp)] += 1;
                        if self.states[locate] != b'T' {
                            self.stats.exec_e_not_front_not_trading += 1;
                        }
                    }
                }
                Body::OrderExecutedWithPrice { .. } => {
                    self.stats.exec_c += 1;
                    if book.at_best(slot) {
                        self.stats.exec_c_at_best += 1;
                        if book.at_head(slot) {
                            self.stats.exec_c_at_front += 1;
                        }
                    }
                }
                _ => {}
            }
        }
        let take = match shares {
            Some(s) if s > order.qty => {
                self.stats.over_reduce += 1;
                order.qty
            }
            Some(s) => s,
            None => order.qty,
        };
        match book.reduce(slot, take) {
            Ok(0) => {
                self.refs.remove(order_ref);
            }
            Ok(_) => {}
            Err(_) => self.stats.book_faults += 1,
        }
        if self.check {
            self.after_update(locate, m.header.timestamp, m.type_byte());
        }
        Some((take, order.side))
    }

    /// The order reference a message will look up, if any.
    #[inline]
    fn target(m: &Message) -> Option<u64> {
        match m.body {
            Body::OrderExecuted { order_ref, .. }
            | Body::OrderExecutedWithPrice { order_ref, .. }
            | Body::OrderCancel { order_ref, .. }
            | Body::OrderDelete { order_ref } => Some(order_ref),
            Body::OrderReplace { old_ref, .. } => Some(old_ref),
            _ => None,
        }
    }

    /// First hint for a message that will be applied soon: start loading the
    /// index entries it needs. None of the three hints changes any state, so
    /// a replay gives the same books with or without them.
    #[inline]
    pub fn prefetch_index(&self, m: &Message) {
        match m.body {
            Body::AddOrder { order_ref, .. } => self.refs.prefetch(order_ref),
            Body::OrderReplace {
                old_ref, new_ref, ..
            } => {
                self.refs.prefetch(old_ref);
                self.refs.prefetch(new_ref);
            }
            _ => {
                if let Some(order_ref) = Self::target(m) {
                    self.refs.prefetch(order_ref);
                }
            }
        }
    }

    /// Second hint: look the order up and start loading it.
    #[inline]
    pub fn prefetch_order(&self, m: &Message) {
        if let Some(r) = Self::target(m).and_then(|order_ref| self.refs.get(order_ref)) {
            if let Some(book) = self.books.get(usize::from(r.locate)) {
                book.prefetch(r.handle);
            }
        }
    }

    /// Third hint, for messages that may remove the order: start loading its
    /// neighbours in the queue.
    #[inline]
    pub fn prefetch_links(&self, m: &Message) {
        if matches!(m.body, Body::OrderCancel { .. }) {
            return;
        }
        if let Some(r) = Self::target(m).and_then(|order_ref| self.refs.get(order_ref)) {
            if let Some(book) = self.books.get(usize::from(r.locate)) {
                book.prefetch_links(r.handle);
            }
        }
    }

    /// Apply one message.
    #[inline]
    pub fn apply(&mut self, m: &Message) {
        let ts = m.header.timestamp;
        if self.stats.messages == 0 {
            self.stats.first_ts = ts;
        } else if ts < self.stats.last_ts {
            self.stats.time_reversals += 1;
        }
        self.stats.last_ts = ts;
        self.stats.messages += 1;
        self.stats.counts[usize::from(m.type_byte())] += 1;
        match m.body {
            Body::AddOrder {
                order_ref,
                side,
                shares,
                price,
                ..
            } => self.add(m, order_ref, side, shares, price),
            Body::OrderExecuted {
                order_ref, shares, ..
            } => {
                if let Some((taken, _)) = self.reduce(m, order_ref, Some(shares)) {
                    self.stats.shares_executed += u64::from(taken);
                    self.stats.printed_shares += u64::from(taken);
                }
            }
            Body::OrderExecutedWithPrice {
                order_ref,
                shares,
                printable,
                ..
            } => {
                if let Some((taken, _)) = self.reduce(m, order_ref, Some(shares)) {
                    self.stats.shares_executed += u64::from(taken);
                    if printable == b'N' {
                        self.stats.non_printable_shares += u64::from(taken);
                    } else {
                        self.stats.printed_shares += u64::from(taken);
                    }
                }
            }
            Body::OrderCancel { order_ref, shares } => {
                if let Some((taken, _)) = self.reduce(m, order_ref, Some(shares)) {
                    self.stats.shares_cancelled += u64::from(taken);
                }
            }
            Body::OrderDelete { order_ref } => {
                if let Some((taken, _)) = self.reduce(m, order_ref, None) {
                    self.stats.shares_deleted += u64::from(taken);
                }
            }
            Body::OrderReplace {
                old_ref,
                new_ref,
                shares,
                price,
            } => {
                // The replacement keeps the side and stock of the original
                // (section 1.4.5) and joins the back of the queue at its price.
                if self.policy == ReplacePolicy::KeepIfSamePriceAndNotLarger
                    && self.replace_in_place(m, old_ref, new_ref, shares, price)
                {
                    return;
                }
                if let Some((taken, side)) = self.reduce(m, old_ref, None) {
                    self.stats.shares_replaced += u64::from(taken);
                    self.add(m, new_ref, side, shares, price);
                }
            }
            Body::Trade { shares, .. } => {
                self.stats.printed_shares += u64::from(shares);
            }
            Body::CrossTrade { shares, .. } => {
                self.stats.cross_shares += shares;
            }
            Body::StockDirectory { stock, .. } => {
                let locate = usize::from(m.header.locate);
                self.ensure(locate);
                self.symbols[locate] = stock;
            }
            Body::TradingAction { state, .. } => {
                let locate = usize::from(m.header.locate);
                self.ensure(locate);
                self.states[locate] = state;
            }
            _ => {}
        }
    }

    /// Close open crossed episodes at the last timestamp, audit every book,
    /// and balance the share ledger.
    pub fn finish(&mut self) -> Audit {
        let last = self.stats.last_ts;
        let mut crossed_at_end = 0;
        for cross in &mut self.cross {
            if cross.since != u64::MAX {
                let ns = last.saturating_sub(cross.since);
                cross.totals.ns += ns;
                self.stats.crossed_ns += ns;
                cross.since = u64::MAX;
            }
        }
        let mut audit = Audit {
            books_with_orders: 0,
            resting_orders: 0,
            resting_shares: 0,
            refs_live: self.refs.len() as u64,
            structure_errors: Vec::new(),
            crossed_at_end: 0,
            ledger_gap: 0,
            digest: self.digest(),
        };
        for (locate, book) in self.books.iter().enumerate() {
            if let Err(e) = book.audit() {
                audit.structure_errors.push(format!("locate {locate}: {e}"));
            }
            if !book.is_empty() {
                audit.books_with_orders += 1;
            }
            if book.is_crossed() {
                crossed_at_end += 1;
            }
            audit.resting_orders += book.order_count() as u64;
            audit.resting_shares += book.resting_qty();
        }
        audit.crossed_at_end = crossed_at_end;
        let s = &self.stats;
        audit.ledger_gap = i128::from(s.shares_added)
            - i128::from(s.shares_executed)
            - i128::from(s.shares_cancelled)
            - i128::from(s.shares_deleted)
            - i128::from(s.shares_replaced)
            - i128::from(audit.resting_shares);
        audit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::itch::{stock, Header};

    fn msg(locate: u16, ts: u64, body: Body) -> Message {
        Message {
            header: Header {
                locate,
                tracking: 0,
                timestamp: ts,
            },
            body,
        }
    }

    fn add(locate: u16, ts: u64, order_ref: u64, side: Side, shares: u32, price: u32) -> Message {
        msg(
            locate,
            ts,
            Body::AddOrder {
                order_ref,
                side,
                shares,
                stock: stock("TEST"),
                price,
                mpid: None,
            },
        )
    }

    #[test]
    fn add_execute_cancel_delete_replace_update_the_book_and_ledger() {
        let mut r = Replay::new(true);
        r.apply(&add(3, 1, 10, Side::Buy, 100, 5000));
        r.apply(&add(3, 2, 11, Side::Buy, 200, 5000));
        r.apply(&add(3, 3, 12, Side::Sell, 300, 5100));
        r.apply(&msg(
            3,
            4,
            Body::OrderExecuted {
                order_ref: 10,
                shares: 40,
                match_number: 1,
            },
        ));
        r.apply(&msg(
            3,
            5,
            Body::OrderCancel {
                order_ref: 11,
                shares: 50,
            },
        ));
        r.apply(&msg(
            3,
            6,
            Body::OrderReplace {
                old_ref: 12,
                new_ref: 13,
                shares: 250,
                price: 5200,
            },
        ));
        r.apply(&msg(3, 7, Body::OrderDelete { order_ref: 10 }));
        let book = r.book(3).unwrap();
        assert_eq!(book.best_bid(), Some(5000));
        assert_eq!(book.best(Side::Buy).unwrap().qty, 150);
        assert_eq!(book.best_ask(), Some(5200));
        assert_eq!(book.best(Side::Sell).unwrap().qty, 250);
        assert_eq!(r.live_orders(), 2);
        assert_eq!(r.stats.shares_added, 850);
        assert_eq!(r.stats.shares_executed, 40);
        assert_eq!(r.stats.shares_cancelled, 50);
        assert_eq!(r.stats.shares_deleted, 60);
        assert_eq!(r.stats.shares_replaced, 300);
        assert_eq!(r.stats.exec_e_at_front, 1);
        let audit = r.finish();
        assert_eq!(audit.resting_shares, 400);
        assert_eq!(audit.ledger_gap, 0);
        assert!(audit.conserved());
    }

    #[test]
    fn replace_loses_time_priority() {
        let mut r = Replay::new(true);
        r.apply(&add(1, 1, 1, Side::Sell, 10, 100));
        r.apply(&add(1, 2, 2, Side::Sell, 10, 100));
        r.apply(&msg(
            1,
            3,
            Body::OrderReplace {
                old_ref: 1,
                new_ref: 3,
                shares: 10,
                price: 100,
            },
        ));
        let ids: Vec<u64> = r
            .book(1)
            .unwrap()
            .queue(Side::Sell, 100)
            .map(|o| o.id)
            .collect();
        assert_eq!(ids, [2, 3]);
    }

    #[test]
    fn the_alternative_replace_policy_keeps_the_place_only_when_it_should() {
        let mut r = Replay::new(true).with_policy(ReplacePolicy::KeepIfSamePriceAndNotLarger);
        r.apply(&add(1, 1, 1, Side::Sell, 10, 100));
        r.apply(&add(1, 2, 2, Side::Sell, 10, 100));
        let replace = |old_ref, new_ref, shares, price| {
            msg(
                1,
                3,
                Body::OrderReplace {
                    old_ref,
                    new_ref,
                    shares,
                    price,
                },
            )
        };
        let queue = |r: &Replay| -> Vec<(u64, u32)> {
            r.book(1)
                .unwrap()
                .queue(Side::Sell, 100)
                .map(|o| (o.id, o.qty))
                .collect()
        };
        // Same price, fewer shares: order 1 keeps the front as order 3.
        r.apply(&replace(1, 3, 6, 100));
        assert_eq!(queue(&r), [(3, 6), (2, 10)]);
        // More shares: back of the queue.
        r.apply(&replace(3, 4, 7, 100));
        assert_eq!(queue(&r), [(2, 10), (4, 7)]);
        // New price: a new level.
        r.apply(&replace(2, 5, 10, 101));
        assert_eq!(queue(&r), [(4, 7)]);
        assert_eq!(r.live_orders(), 2);
        let audit = r.finish();
        assert!(audit.conserved());
        assert_eq!(r.stats.shares_added, 43);
        assert_eq!(r.stats.shares_replaced, 26);
    }

    #[test]
    fn anomalies_are_counted_not_fatal() {
        let mut r = Replay::new(true);
        r.apply(&add(1, 1, 1, Side::Buy, 10, 100));
        r.apply(&add(1, 2, 1, Side::Buy, 10, 100));
        r.apply(&add(1, 3, 2, Side::Buy, 0, 100));
        r.apply(&msg(1, 4, Body::OrderDelete { order_ref: 99 }));
        r.apply(&msg(
            2,
            5,
            Body::OrderExecuted {
                order_ref: 1,
                shares: 15,
                match_number: 1,
            },
        ));
        r.apply(&msg(
            1,
            4,
            Body::OrderReplace {
                old_ref: 77,
                new_ref: 78,
                shares: 1,
                price: 1,
            },
        ));
        assert_eq!(r.stats.duplicate_ref, 1);
        assert_eq!(r.stats.zero_share_add, 1);
        assert_eq!(r.stats.unknown_ref, 2);
        assert_eq!(r.stats.over_reduce, 1);
        assert_eq!(r.stats.locate_mismatch, 1);
        assert_eq!(r.stats.time_reversals, 1);
        assert_eq!(r.stats.shares_executed, 10);
        assert!(r.finish().conserved());
    }

    #[test]
    fn crossed_books_are_detected_and_timed() {
        let mut r = Replay::new(true);
        r.apply(&msg(
            1,
            0,
            Body::TradingAction {
                stock: stock("TEST"),
                state: b'H',
                reserved: b' ',
                reason: *b"    ",
            },
        ));
        r.apply(&add(1, 100, 1, Side::Buy, 10, 100));
        r.apply(&add(1, 200, 2, Side::Sell, 10, 100));
        assert_eq!(r.stats.locked_updates, 1);
        assert_eq!(r.stats.crossed_episodes, 0);
        r.apply(&add(1, 300, 3, Side::Sell, 10, 99));
        r.apply(&add(1, 400, 4, Side::Sell, 10, 98));
        assert_eq!(r.stats.crossed_episodes, 1);
        assert_eq!(r.stats.crossed_updates, 2);
        assert_eq!(r.stats.crossed_episodes_trading, 0);
        r.apply(&msg(1, 900, Body::OrderDelete { order_ref: 1 }));
        assert_eq!(r.stats.crossed_ns, 600);
        assert_eq!(
            r.examples[0],
            CrossExample {
                timestamp: 300,
                locate: 1,
                bid: 100,
                ask: 99,
                state: b'H',
                msg_type: b'A',
            }
        );
        assert_eq!(r.cross_totals().len(), 1);
    }

    #[test]
    fn checks_off_gives_the_same_books() {
        let stream = [
            add(1, 1, 1, Side::Buy, 10, 100),
            add(1, 2, 2, Side::Sell, 10, 101),
            msg(
                1,
                3,
                Body::OrderExecuted {
                    order_ref: 2,
                    shares: 4,
                    match_number: 1,
                },
            ),
        ];
        let mut on = Replay::new(true);
        let mut off = Replay::new(false);
        for m in &stream {
            on.apply(m);
            off.apply(m);
        }
        assert_eq!(on.digest(), off.digest());
        assert_eq!(off.stats.book_updates, 0);
        assert_eq!(on.stats.book_updates, 3);
    }

    #[test]
    fn printed_and_cross_volume_follow_the_specification() {
        let mut r = Replay::new(false);
        r.apply(&add(1, 1, 1, Side::Buy, 100, 100));
        r.apply(&msg(
            1,
            2,
            Body::OrderExecutedWithPrice {
                order_ref: 1,
                shares: 30,
                match_number: 1,
                printable: b'N',
                price: 100,
            },
        ));
        r.apply(&msg(
            1,
            3,
            Body::OrderExecutedWithPrice {
                order_ref: 1,
                shares: 20,
                match_number: 2,
                printable: b'Y',
                price: 99,
            },
        ));
        r.apply(&msg(
            1,
            4,
            Body::Trade {
                order_ref: 0,
                side: b'B',
                shares: 7,
                stock: stock("TEST"),
                price: 100,
                match_number: 3,
            },
        ));
        r.apply(&msg(
            1,
            5,
            Body::CrossTrade {
                shares: 1000,
                stock: stock("TEST"),
                price: 100,
                match_number: 4,
                cross_type: b'C',
            },
        ));
        assert_eq!(r.stats.printed_shares, 27);
        assert_eq!(r.stats.non_printable_shares, 30);
        assert_eq!(r.stats.cross_shares, 1000);
        assert_eq!(r.stats.shares_executed, 50);
    }
}
