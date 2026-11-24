//! A price-time-priority limit order book for one instrument.
//!
//! Orders live in a slab and are linked into one first-in-first-out queue per
//! price level. Each side keeps its levels in a vector sorted so that the best
//! price is the last element, which makes the common case (activity at or near
//! the top of the book) a short scan from the end.
//!
//! A resting order is named by a [`Handle`]: a slot number and the slot's
//! generation. Releasing a slot bumps its generation, so a handle kept past
//! the life of its order is refused with [`BookError::StaleHandle`] instead
//! of reaching whatever order took the slot next. The check is one compare
//! on a cache line the operation reads anyway.
//!
//! The book is passive: `add` never matches. Matching lives in
//! [`crate::engine::Engine`]; replay of an exchange feed lives in
//! [`crate::replay::Replay`], where the exchange has already done the matching.

/// Sentinel for "no order" in the intrusive queue links.
const NIL: u32 = u32::MAX;

/// Names one resting order in one [`Book`].
///
/// A handle is only meaningful to the book that issued it. It stops working
/// when its order's quantity reaches zero. A slot's generation wraps after
/// 2^32 releases of that one slot, which is the only way a stale handle could
/// be accepted again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Handle {
    index: u32,
    generation: u32,
}

/// Why the book refused a request. The book is unchanged when it refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookError {
    /// An order must have positive quantity.
    ZeroQuantity,
    /// The handle names an order that no longer rests (or never did).
    StaleHandle,
    /// A reduction asked for more than the order has left.
    ExceedsRemaining {
        /// Quantity asked for.
        asked: u32,
        /// Quantity available.
        remaining: u32,
    },
}

impl std::fmt::Display for BookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BookError::ZeroQuantity => write!(f, "an order must have positive quantity"),
            BookError::StaleHandle => write!(f, "the handle's order no longer rests"),
            BookError::ExceedsRemaining { asked, remaining } => {
                write!(f, "asked for {asked} but only {remaining} remain")
            }
        }
    }
}

impl std::error::Error for BookError {}

/// How many levels to scan from the top of the book before falling back to a
/// binary search.
const PROBE: usize = 4;

/// Side of the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// Bid side.
    Buy,
    /// Offer side.
    Sell,
}

impl Side {
    /// The other side.
    #[inline]
    pub fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Order {
    id: u64,
    price: u32,
    /// Remaining quantity. Zero marks a free slot.
    qty: u32,
    prev: u32,
    next: u32,
    /// Bumped every time the slot is released.
    generation: u32,
    side: Side,
}

#[derive(Debug, Clone, Copy)]
struct Level {
    price: u32,
    qty: u64,
    head: u32,
    tail: u32,
    count: u32,
}

/// A read-only view of one resting order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderView {
    /// Caller's identifier (the ITCH order reference number in replay).
    pub id: u64,
    /// Side the order rests on.
    pub side: Side,
    /// Limit price in ticks (ITCH Price(4): ten-thousandths of a dollar).
    pub price: u32,
    /// Remaining quantity.
    pub qty: u32,
}

/// A read-only view of one price level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelView {
    /// Level price.
    pub price: u32,
    /// Sum of the remaining quantity of every order at this price.
    pub qty: u64,
    /// Number of orders at this price.
    pub count: u32,
}

/// One instrument's book.
#[derive(Debug, Clone, Default)]
pub struct Book {
    orders: Vec<Order>,
    free: Vec<u32>,
    /// Ascending by price; the best bid is last.
    bids: Vec<Level>,
    /// Descending by price; the best ask is last.
    asks: Vec<Level>,
}

#[inline]
fn find(levels: &[Level], side: Side, price: u32) -> Result<usize, usize> {
    let n = levels.len();
    let lo = n.saturating_sub(PROBE);
    let mut i = n;
    while i > lo {
        i -= 1;
        if levels[i].price == price {
            return Ok(i);
        }
    }
    match side {
        Side::Buy => levels.binary_search_by(|l| l.price.cmp(&price)),
        Side::Sell => levels.binary_search_by(|l| price.cmp(&l.price)),
    }
}

impl Book {
    /// An empty book.
    pub fn new() -> Self {
        Self::default()
    }

    /// The live order a handle names.
    #[inline]
    fn live(&self, handle: Handle) -> Result<&Order, BookError> {
        match self.orders.get(handle.index as usize) {
            Some(o) if o.generation == handle.generation && o.qty > 0 => Ok(o),
            _ => Err(BookError::StaleHandle),
        }
    }

    /// Hint that the order `handle` names is about to be used. Changes
    /// nothing and is safe with a stale handle.
    #[inline]
    pub fn prefetch(&self, handle: Handle) {
        if let Some(o) = self.orders.get(handle.index as usize) {
            crate::mem::prefetch(o);
        }
    }

    /// Hint that the order `handle` names is about to be removed: start
    /// loading its neighbours in the queue and the best level on its side.
    /// Changes nothing and is safe with a stale handle.
    #[inline]
    pub fn prefetch_links(&self, handle: Handle) {
        let Some(o) = self.orders.get(handle.index as usize) else {
            return;
        };
        if let Some(prev) = self.orders.get(o.prev as usize) {
            crate::mem::prefetch(prev);
        }
        if let Some(next) = self.orders.get(o.next as usize) {
            crate::mem::prefetch(next);
        }
        if let Some(level) = self.side_levels(o.side).last() {
            crate::mem::prefetch(level);
        }
    }

    /// Rest a new order at the back of the queue at its price.
    ///
    /// Returns a handle that stays valid until the order's quantity reaches
    /// zero. This never matches, so the caller decides whether a crossing
    /// price is legal. A zero quantity is refused.
    #[inline]
    pub fn add(&mut self, id: u64, side: Side, price: u32, qty: u32) -> Result<Handle, BookError> {
        if qty == 0 {
            return Err(BookError::ZeroQuantity);
        }
        let (slot, generation) = match self.free.pop() {
            Some(s) => {
                let o = &mut self.orders[s as usize];
                let generation = o.generation;
                *o = Order {
                    id,
                    price,
                    qty,
                    prev: NIL,
                    next: NIL,
                    generation,
                    side,
                };
                (s, generation)
            }
            None => {
                self.orders.push(Order {
                    id,
                    price,
                    qty,
                    prev: NIL,
                    next: NIL,
                    generation: 0,
                    side,
                });
                ((self.orders.len() - 1) as u32, 0)
            }
        };
        let levels = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let pos = match find(levels, side, price) {
            Ok(i) => i,
            Err(i) => {
                levels.insert(
                    i,
                    Level {
                        price,
                        qty: 0,
                        head: NIL,
                        tail: NIL,
                        count: 0,
                    },
                );
                i
            }
        };
        let level = &mut levels[pos];
        let tail = level.tail;
        level.qty += u64::from(qty);
        level.count += 1;
        level.tail = slot;
        if tail == NIL {
            level.head = slot;
        } else {
            self.orders[tail as usize].next = slot;
            self.orders[slot as usize].prev = tail;
        }
        Ok(Handle {
            index: slot,
            generation,
        })
    }

    /// Take `qty` off a resting order, keeping its place in the queue.
    ///
    /// Returns the remaining quantity. At zero the order is removed and its
    /// handle stops working. A stale handle, or a `qty` above what the order
    /// has left, is refused and changes nothing.
    #[inline]
    pub fn reduce(&mut self, handle: Handle, qty: u32) -> Result<u32, BookError> {
        let order = self.live(handle)?;
        if qty > order.qty {
            return Err(BookError::ExceedsRemaining {
                asked: qty,
                remaining: order.qty,
            });
        }
        let slot = handle.index;
        let (side, price, remaining, prev, next) = (
            order.side,
            order.price,
            order.qty - qty,
            order.prev,
            order.next,
        );
        self.orders[slot as usize].qty = remaining;
        let levels = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let pos = find(levels, side, price).expect("a resting order has a level");
        let level = &mut levels[pos];
        level.qty -= u64::from(qty);
        if remaining == 0 {
            level.count -= 1;
            if prev == NIL {
                level.head = next;
            } else {
                self.orders[prev as usize].next = next;
            }
            if next == NIL {
                level.tail = prev;
            } else {
                self.orders[next as usize].prev = prev;
            }
            if level.count == 0 {
                levels.remove(pos);
            }
            let o = &mut self.orders[slot as usize];
            o.generation = o.generation.wrapping_add(1);
            self.free.push(slot);
        }
        Ok(remaining)
    }

    /// Move a resting order to the front of the queue at its price.
    pub fn move_to_front(&mut self, handle: Handle) -> Result<(), BookError> {
        let o = self.live(handle)?;
        let (side, price, prev, next) = (o.side, o.price, o.prev, o.next);
        if prev == NIL {
            return Ok(());
        }
        let slot = handle.index;
        let levels = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let pos = find(levels, side, price).expect("a resting order has a level");
        let level = &mut levels[pos];
        self.orders[prev as usize].next = next;
        if next == NIL {
            level.tail = prev;
        } else {
            self.orders[next as usize].prev = prev;
        }
        let head = level.head;
        level.head = slot;
        self.orders[head as usize].prev = slot;
        self.orders[slot as usize].prev = NIL;
        self.orders[slot as usize].next = head;
        Ok(())
    }

    /// Give a resting order a new identifier without moving it in its queue.
    pub fn set_id(&mut self, handle: Handle, id: u64) -> Result<(), BookError> {
        self.live(handle)?;
        self.orders[handle.index as usize].id = id;
        Ok(())
    }

    /// Remove a resting order outright. Returns the quantity it still had.
    #[inline]
    pub fn remove(&mut self, handle: Handle) -> Result<u32, BookError> {
        let qty = self.live(handle)?.qty;
        self.reduce(handle, qty)?;
        Ok(qty)
    }

    /// The order a handle names, or `None` if it no longer rests.
    #[inline]
    pub fn get(&self, handle: Handle) -> Option<OrderView> {
        self.live(handle).ok().map(|o| OrderView {
            id: o.id,
            side: o.side,
            price: o.price,
            qty: o.qty,
        })
    }

    #[inline]
    fn side_levels(&self, side: Side) -> &[Level] {
        match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        }
    }

    /// Best level on `side`, if any.
    #[inline]
    pub fn best(&self, side: Side) -> Option<LevelView> {
        self.side_levels(side).last().map(|l| LevelView {
            price: l.price,
            qty: l.qty,
            count: l.count,
        })
    }

    /// Highest bid price.
    #[inline]
    pub fn best_bid(&self) -> Option<u32> {
        self.bids.last().map(|l| l.price)
    }

    /// Lowest ask price.
    #[inline]
    pub fn best_ask(&self) -> Option<u32> {
        self.asks.last().map(|l| l.price)
    }

    /// The oldest order at the best price on `side`.
    #[inline]
    pub fn front(&self, side: Side) -> Option<Handle> {
        self.side_levels(side).last().map(|l| Handle {
            index: l.head,
            generation: self.orders[l.head as usize].generation,
        })
    }

    /// True when the best bid is strictly above the best ask.
    #[inline]
    pub fn is_crossed(&self) -> bool {
        match (self.bids.last(), self.asks.last()) {
            (Some(b), Some(a)) => b.price > a.price,
            _ => false,
        }
    }

    /// True when the best bid equals the best ask.
    #[inline]
    pub fn is_locked(&self) -> bool {
        match (self.bids.last(), self.asks.last()) {
            (Some(b), Some(a)) => b.price == a.price,
            _ => false,
        }
    }

    /// True when the order rests at the best price on its side. False for a
    /// stale handle.
    #[inline]
    pub fn at_best(&self, handle: Handle) -> bool {
        self.live(handle).is_ok_and(|o| {
            self.side_levels(o.side)
                .last()
                .is_some_and(|l| l.price == o.price)
        })
    }

    /// True when the order is the oldest at its price. False for a stale
    /// handle.
    #[inline]
    pub fn at_head(&self, handle: Handle) -> bool {
        self.live(handle).is_ok_and(|o| o.prev == NIL)
    }

    /// Levels on `side`, best first.
    pub fn levels(&self, side: Side) -> impl Iterator<Item = LevelView> + '_ {
        self.side_levels(side).iter().rev().map(|l| LevelView {
            price: l.price,
            qty: l.qty,
            count: l.count,
        })
    }

    /// Orders at `price` on `side`, oldest first.
    pub fn queue(&self, side: Side, price: u32) -> impl Iterator<Item = OrderView> + '_ {
        let levels = self.side_levels(side);
        let mut slot = match find(levels, side, price) {
            Ok(i) => levels[i].head,
            Err(_) => NIL,
        };
        std::iter::from_fn(move || {
            if slot == NIL {
                return None;
            }
            let o = &self.orders[slot as usize];
            slot = o.next;
            Some(OrderView {
                id: o.id,
                side: o.side,
                price: o.price,
                qty: o.qty,
            })
        })
    }

    /// Number of resting orders.
    #[inline]
    pub fn order_count(&self) -> usize {
        self.orders.len() - self.free.len()
    }

    /// Number of price levels on `side`.
    #[inline]
    pub fn level_count(&self, side: Side) -> usize {
        self.side_levels(side).len()
    }

    /// True when nothing rests on either side.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.bids.is_empty() && self.asks.is_empty()
    }

    /// Total resting quantity on both sides, from the level aggregates.
    pub fn resting_qty(&self) -> u64 {
        self.bids
            .iter()
            .chain(self.asks.iter())
            .map(|l| l.qty)
            .sum()
    }

    /// Walk every structure and check it against the others.
    ///
    /// Checks: level prices strictly ordered, no empty level, each level's
    /// aggregate quantity and count equal to its queue, queue links consistent
    /// in both directions, each order on the right side at the right price with
    /// positive quantity, and the number of queued orders equal to the number
    /// of live slots. Cost is linear in the size of the book.
    pub fn audit(&self) -> Result<(), String> {
        let mut seen = 0usize;
        for side in [Side::Buy, Side::Sell] {
            let levels = self.side_levels(side);
            for pair in levels.windows(2) {
                let ordered = match side {
                    Side::Buy => pair[0].price < pair[1].price,
                    Side::Sell => pair[0].price > pair[1].price,
                };
                if !ordered {
                    return Err(format!("{side:?} levels out of order at {}", pair[1].price));
                }
            }
            for level in levels {
                if level.count == 0 {
                    return Err(format!("{side:?} level {} is empty", level.price));
                }
                let (mut qty, mut count, mut prev, mut slot) = (0u64, 0u32, NIL, level.head);
                while slot != NIL {
                    let o = self
                        .orders
                        .get(slot as usize)
                        .ok_or_else(|| format!("slot {slot} out of range"))?;
                    if o.side != side || o.price != level.price {
                        return Err(format!("order {} queued at the wrong level", o.id));
                    }
                    if o.qty == 0 {
                        return Err(format!("order {} rests with zero quantity", o.id));
                    }
                    if o.prev != prev {
                        return Err(format!("order {} has a broken back link", o.id));
                    }
                    qty += u64::from(o.qty);
                    count += 1;
                    if count > level.count {
                        return Err(format!("{side:?} level {} queue too long", level.price));
                    }
                    prev = slot;
                    slot = o.next;
                }
                if prev != level.tail {
                    return Err(format!("{side:?} level {} has a wrong tail", level.price));
                }
                if qty != level.qty || count != level.count {
                    return Err(format!(
                        "{side:?} level {} aggregate {}x{} but queue holds {}x{}",
                        level.price, level.qty, level.count, qty, count
                    ));
                }
                seen += count as usize;
            }
        }
        if seen != self.order_count() {
            return Err(format!(
                "{} orders queued but {} slots live",
                seen,
                self.order_count()
            ));
        }
        Ok(())
    }

    /// Feed the full state of the book into a digest, in a fixed order.
    pub fn digest_into(&self, h: &mut Fnv) {
        for side in [Side::Buy, Side::Sell] {
            h.write_u64(self.level_count(side) as u64);
            for level in self.side_levels(side).iter().rev() {
                h.write_u64(u64::from(level.price));
                h.write_u64(level.qty);
                h.write_u64(u64::from(level.count));
                let mut slot = level.head;
                while slot != NIL {
                    let o = &self.orders[slot as usize];
                    h.write_u64(o.id);
                    h.write_u64(u64::from(o.qty));
                    slot = o.next;
                }
            }
        }
    }
}

/// 64-bit FNV-1a, used to fingerprint book state for the golden tests.
#[derive(Debug, Clone, Copy)]
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self::new()
    }
}

impl Fnv {
    /// Start a digest at the FNV offset basis.
    pub fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    /// Mix in eight bytes, little end first.
    #[inline]
    pub fn write_u64(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    /// The digest so far.
    pub fn finish(self) -> u64 {
        self.0
    }
}

/// Digest a set of books keyed by an integer (the stock locate in replay).
/// Empty books are skipped, so sparse and dense containers agree.
pub fn digest_books<'a>(books: impl Iterator<Item = (u64, &'a Book)>) -> u64 {
    let mut h = Fnv::new();
    for (key, book) in books {
        if book.is_empty() {
            continue;
        }
        h.write_u64(key);
        book.digest_into(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(b: &mut Book, id: u64, side: Side, price: u32, qty: u32) -> Handle {
        b.add(id, side, price, qty).unwrap()
    }

    #[test]
    fn add_builds_levels_best_last() {
        let mut b = Book::new();
        add(&mut b, 1, Side::Buy, 100, 10);
        add(&mut b, 2, Side::Buy, 102, 5);
        add(&mut b, 3, Side::Buy, 101, 7);
        add(&mut b, 4, Side::Sell, 105, 1);
        add(&mut b, 5, Side::Sell, 103, 2);
        add(&mut b, 6, Side::Sell, 104, 3);
        assert_eq!(b.best_bid(), Some(102));
        assert_eq!(b.best_ask(), Some(103));
        let bids: Vec<u32> = b.levels(Side::Buy).map(|l| l.price).collect();
        let asks: Vec<u32> = b.levels(Side::Sell).map(|l| l.price).collect();
        assert_eq!(bids, [102, 101, 100]);
        assert_eq!(asks, [103, 104, 105]);
        assert!(!b.is_crossed());
        b.audit().unwrap();
    }

    #[test]
    fn queue_is_first_in_first_out() {
        let mut b = Book::new();
        let s1 = add(&mut b, 1, Side::Sell, 50, 10);
        let s2 = add(&mut b, 2, Side::Sell, 50, 20);
        let s3 = add(&mut b, 3, Side::Sell, 50, 30);
        assert_eq!(b.front(Side::Sell), Some(s1));
        assert!(b.at_head(s1) && !b.at_head(s2) && !b.at_head(s3));
        let ids: Vec<u64> = b.queue(Side::Sell, 50).map(|o| o.id).collect();
        assert_eq!(ids, [1, 2, 3]);
        assert_eq!(
            b.best(Side::Sell),
            Some(LevelView {
                price: 50,
                qty: 60,
                count: 3
            })
        );
        // Removing the middle order keeps the others in order.
        assert_eq!(b.remove(s2), Ok(20));
        let ids: Vec<u64> = b.queue(Side::Sell, 50).map(|o| o.id).collect();
        assert_eq!(ids, [1, 3]);
        b.audit().unwrap();
    }

    #[test]
    fn partial_reduce_keeps_priority() {
        let mut b = Book::new();
        let s1 = add(&mut b, 1, Side::Buy, 10, 100);
        add(&mut b, 2, Side::Buy, 10, 100);
        assert_eq!(b.reduce(s1, 40), Ok(60));
        assert_eq!(b.front(Side::Buy), Some(s1));
        assert_eq!(b.best(Side::Buy).unwrap().qty, 160);
        b.audit().unwrap();
    }

    #[test]
    fn emptying_a_level_removes_it_and_reuses_the_slot() {
        let mut b = Book::new();
        let s1 = add(&mut b, 1, Side::Buy, 10, 5);
        add(&mut b, 2, Side::Buy, 9, 5);
        assert_eq!(b.reduce(s1, 5), Ok(0));
        assert_eq!(b.best_bid(), Some(9));
        assert_eq!(b.order_count(), 1);
        let s3 = add(&mut b, 3, Side::Sell, 11, 1);
        // Same slot, new generation: the handles differ.
        assert_eq!(s3.index, s1.index);
        assert_ne!(s3, s1);
        assert_eq!(b.get(s3).unwrap().id, 3);
        b.audit().unwrap();
    }

    #[test]
    fn a_stale_handle_is_refused_and_changes_nothing() {
        let mut b = Book::new();
        let old = add(&mut b, 1, Side::Buy, 10, 5);
        assert_eq!(b.remove(old), Ok(5));
        // The slot is reused by a different order at a different price.
        let new = add(&mut b, 2, Side::Sell, 20, 7);
        assert_eq!(new.index, old.index);
        let before = digest_books([(1u64, &b)].into_iter());
        assert_eq!(b.reduce(old, 1), Err(BookError::StaleHandle));
        assert_eq!(b.remove(old), Err(BookError::StaleHandle));
        assert_eq!(b.set_id(old, 9), Err(BookError::StaleHandle));
        assert_eq!(b.move_to_front(old), Err(BookError::StaleHandle));
        assert_eq!(b.get(old), None);
        assert!(!b.at_best(old) && !b.at_head(old));
        assert_eq!(digest_books([(1u64, &b)].into_iter()), before);
        assert_eq!(b.get(new).unwrap().qty, 7);
        b.audit().unwrap();
        // A handle from nowhere is refused too.
        let bogus = Handle {
            index: 1_000,
            generation: 0,
        };
        assert_eq!(b.reduce(bogus, 1), Err(BookError::StaleHandle));
    }

    #[test]
    fn zero_quantity_and_over_reduce_are_errors() {
        let mut b = Book::new();
        assert_eq!(b.add(1, Side::Buy, 1, 0), Err(BookError::ZeroQuantity));
        assert!(b.is_empty());
        let s = add(&mut b, 1, Side::Buy, 1, 1);
        assert_eq!(
            b.reduce(s, 2),
            Err(BookError::ExceedsRemaining {
                asked: 2,
                remaining: 1
            })
        );
        assert_eq!(b.get(s).unwrap().qty, 1);
        // Taking nothing is allowed and changes nothing.
        assert_eq!(b.reduce(s, 0), Ok(1));
        b.audit().unwrap();
    }

    #[test]
    fn crossed_and_locked_are_reported() {
        let mut b = Book::new();
        add(&mut b, 1, Side::Buy, 100, 1);
        let ask = add(&mut b, 2, Side::Sell, 100, 1);
        assert!(b.is_locked() && !b.is_crossed());
        b.remove(ask).unwrap();
        add(&mut b, 3, Side::Sell, 99, 1);
        assert!(b.is_crossed() && !b.is_locked());
    }

    #[test]
    fn deep_levels_are_found_by_binary_search() {
        let mut b = Book::new();
        let slots: Vec<Handle> = (0..100u32)
            .map(|i| add(&mut b, u64::from(i), Side::Sell, 1000 + i, 1))
            .collect();
        assert_eq!(b.best_ask(), Some(1000));
        // The deepest level is far outside the probe window.
        assert!(!b.at_best(slots[99]));
        assert_eq!(b.remove(slots[99]), Ok(1));
        assert_eq!(b.level_count(Side::Sell), 99);
        b.audit().unwrap();
    }

    #[test]
    fn move_to_front_reorders_one_queue() {
        let mut b = Book::new();
        let s1 = add(&mut b, 1, Side::Buy, 10, 1);
        let s2 = add(&mut b, 2, Side::Buy, 10, 2);
        let s3 = add(&mut b, 3, Side::Buy, 10, 3);
        let ids = |b: &Book| -> Vec<u64> { b.queue(Side::Buy, 10).map(|o| o.id).collect() };
        b.move_to_front(s3).unwrap();
        assert_eq!(ids(&b), [3, 1, 2]);
        b.audit().unwrap();
        b.move_to_front(s1).unwrap();
        assert_eq!(ids(&b), [1, 3, 2]);
        b.move_to_front(s1).unwrap();
        assert_eq!(ids(&b), [1, 3, 2]);
        b.audit().unwrap();
        assert_eq!(b.front(Side::Buy), Some(s1));
        assert!(b.at_head(s1) && !b.at_head(s2));
    }

    #[test]
    fn digest_depends_on_queue_order() {
        let mut a = Book::new();
        add(&mut a, 1, Side::Buy, 10, 5);
        add(&mut a, 2, Side::Buy, 10, 5);
        let mut b = Book::new();
        add(&mut b, 2, Side::Buy, 10, 5);
        add(&mut b, 1, Side::Buy, 10, 5);
        let da = digest_books([(1u64, &a)].into_iter());
        let db = digest_books([(1u64, &b)].into_iter());
        assert_ne!(da, db);
        assert_eq!(da, digest_books([(1u64, &a.clone())].into_iter()));
    }
}
