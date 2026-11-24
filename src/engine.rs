//! A matching engine on top of [`Book`]: price-time priority, limit orders.
//!
//! An incoming order trades against the opposite side while its limit crosses
//! the best opposite price, taking the oldest order at each price first, and
//! rests whatever is left. A book driven only through this engine is never
//! crossed or locked.

use std::collections::HashMap;

use crate::book::{Book, BookError, Handle, Side};

/// One match between an incoming order and a resting order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fill {
    /// Identifier of the resting order.
    pub resting_id: u64,
    /// Trade price: the resting order's price.
    pub price: u32,
    /// Quantity traded.
    pub qty: u32,
    /// What the resting order has left after this fill.
    pub resting_remaining: u32,
}

/// Why the engine refused a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineError {
    /// An order with this identifier is already resting.
    DuplicateId(u64),
    /// No resting order has this identifier.
    UnknownId(u64),
    /// Orders and reductions must have positive quantity.
    ZeroQty,
    /// A partial cancel asked for more than the order has left.
    ExceedsRemaining {
        /// Quantity asked for.
        asked: u32,
        /// Quantity available.
        remaining: u32,
    },
    /// The book refused a request the engine thought was valid. This would
    /// be a fault in the engine.
    Book(BookError),
}

impl From<BookError> for EngineError {
    fn from(e: BookError) -> Self {
        EngineError::Book(e)
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::DuplicateId(id) => write!(f, "order {id} already rests"),
            EngineError::UnknownId(id) => write!(f, "order {id} is not resting"),
            EngineError::ZeroQty => write!(f, "quantity must be positive"),
            EngineError::ExceedsRemaining { asked, remaining } => {
                write!(f, "asked for {asked} but only {remaining} remain")
            }
            EngineError::Book(e) => write!(f, "book fault: {e}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// A single-instrument matching engine.
#[derive(Debug, Clone, Default)]
pub struct Engine {
    book: Book,
    slots: HashMap<u64, Handle>,
}

impl Engine {
    /// An engine with an empty book.
    pub fn new() -> Self {
        Self::default()
    }

    /// The book, read-only.
    pub fn book(&self) -> &Book {
        &self.book
    }

    /// Remaining quantity of a resting order.
    pub fn remaining(&self, id: u64) -> Option<u32> {
        self.slots
            .get(&id)
            .and_then(|&s| self.book.get(s))
            .map(|o| o.qty)
    }

    /// Side and price of a resting order.
    pub fn resting(&self, id: u64) -> Option<(Side, u32)> {
        self.slots
            .get(&id)
            .and_then(|&s| self.book.get(s))
            .map(|o| (o.side, o.price))
    }

    /// Submit a limit order. Fills are appended to `fills` in the order they
    /// happen. Returns the quantity left resting under `id` (zero if the order
    /// traded in full).
    pub fn submit(
        &mut self,
        id: u64,
        side: Side,
        price: u32,
        qty: u32,
        fills: &mut Vec<Fill>,
    ) -> Result<u32, EngineError> {
        if qty == 0 {
            return Err(EngineError::ZeroQty);
        }
        if self.slots.contains_key(&id) {
            return Err(EngineError::DuplicateId(id));
        }
        let mut left = qty;
        let against = side.opposite();
        while left > 0 {
            let Some(slot) = self.book.front(against) else {
                break;
            };
            let resting = self.book.get(slot).ok_or(BookError::StaleHandle)?;
            let crosses = match side {
                Side::Buy => resting.price <= price,
                Side::Sell => resting.price >= price,
            };
            if !crosses {
                break;
            }
            let traded = left.min(resting.qty);
            let resting_remaining = self.book.reduce(slot, traded)?;
            if resting_remaining == 0 {
                self.slots.remove(&resting.id);
            }
            left -= traded;
            fills.push(Fill {
                resting_id: resting.id,
                price: resting.price,
                qty: traded,
                resting_remaining,
            });
        }
        if left > 0 {
            let slot = self.book.add(id, side, price, left)?;
            self.slots.insert(id, slot);
        }
        Ok(left)
    }

    /// Take `qty` off a resting order without losing its place. Returns what
    /// is left.
    pub fn cancel(&mut self, id: u64, qty: u32) -> Result<u32, EngineError> {
        if qty == 0 {
            return Err(EngineError::ZeroQty);
        }
        let slot = *self.slots.get(&id).ok_or(EngineError::UnknownId(id))?;
        let remaining = self.book.get(slot).ok_or(BookError::StaleHandle)?.qty;
        if qty > remaining {
            return Err(EngineError::ExceedsRemaining {
                asked: qty,
                remaining,
            });
        }
        let left = self.book.reduce(slot, qty)?;
        if left == 0 {
            self.slots.remove(&id);
        }
        Ok(left)
    }

    /// Remove a resting order. Returns the quantity it still had.
    pub fn delete(&mut self, id: u64) -> Result<u32, EngineError> {
        let slot = self.slots.remove(&id).ok_or(EngineError::UnknownId(id))?;
        Ok(self.book.remove(slot)?)
    }

    /// Cancel `old` and submit `new` on the same side with a new price and
    /// quantity. The new order joins the back of its queue and may trade.
    /// Returns the quantity left resting under `new`.
    pub fn replace(
        &mut self,
        old: u64,
        new: u64,
        price: u32,
        qty: u32,
        fills: &mut Vec<Fill>,
    ) -> Result<u32, EngineError> {
        if qty == 0 {
            return Err(EngineError::ZeroQty);
        }
        let slot = *self.slots.get(&old).ok_or(EngineError::UnknownId(old))?;
        if new != old && self.slots.contains_key(&new) {
            return Err(EngineError::DuplicateId(new));
        }
        let side = self.book.get(slot).ok_or(BookError::StaleHandle)?.side;
        self.slots.remove(&old);
        self.book.remove(slot)?;
        self.submit(new, side, price, qty, fills)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_with_asks() -> Engine {
        let mut e = Engine::new();
        let mut fills = Vec::new();
        e.submit(1, Side::Sell, 101, 100, &mut fills).unwrap();
        e.submit(2, Side::Sell, 101, 50, &mut fills).unwrap();
        e.submit(3, Side::Sell, 102, 70, &mut fills).unwrap();
        assert!(fills.is_empty());
        e
    }

    #[test]
    fn passive_order_rests() {
        let mut e = engine_with_asks();
        let mut fills = Vec::new();
        assert_eq!(e.submit(9, Side::Buy, 100, 10, &mut fills), Ok(10));
        assert!(fills.is_empty());
        assert_eq!(e.book().best_bid(), Some(100));
    }

    #[test]
    fn aggressive_order_fills_oldest_first_then_next_price() {
        let mut e = engine_with_asks();
        let mut fills = Vec::new();
        // 180 at a limit of 102: takes 100 from order 1, 50 from order 2,
        // 30 from order 3.
        assert_eq!(e.submit(9, Side::Buy, 102, 180, &mut fills), Ok(0));
        assert_eq!(
            fills,
            [
                Fill {
                    resting_id: 1,
                    price: 101,
                    qty: 100,
                    resting_remaining: 0
                },
                Fill {
                    resting_id: 2,
                    price: 101,
                    qty: 50,
                    resting_remaining: 0
                },
                Fill {
                    resting_id: 3,
                    price: 102,
                    qty: 30,
                    resting_remaining: 40
                },
            ]
        );
        assert_eq!(e.remaining(3), Some(40));
        assert_eq!(e.remaining(9), None);
        e.book().audit().unwrap();
    }

    #[test]
    fn remainder_rests_at_the_limit() {
        let mut e = engine_with_asks();
        let mut fills = Vec::new();
        assert_eq!(e.submit(9, Side::Buy, 101, 200, &mut fills), Ok(50));
        assert_eq!(fills.len(), 2);
        assert_eq!(e.book().best_bid(), Some(101));
        assert_eq!(e.book().best_ask(), Some(102));
        assert!(!e.book().is_crossed() && !e.book().is_locked());
    }

    #[test]
    fn sell_side_matches_highest_bid_first() {
        let mut e = Engine::new();
        let mut fills = Vec::new();
        e.submit(1, Side::Buy, 99, 10, &mut fills).unwrap();
        e.submit(2, Side::Buy, 100, 10, &mut fills).unwrap();
        e.submit(3, Side::Sell, 99, 15, &mut fills).unwrap();
        assert_eq!(fills[0].resting_id, 2);
        assert_eq!(fills[0].price, 100);
        assert_eq!(fills[1].resting_id, 1);
        assert_eq!(fills[1].qty, 5);
    }

    #[test]
    fn cancel_keeps_priority_and_delete_removes() {
        let mut e = engine_with_asks();
        assert_eq!(e.cancel(1, 60), Ok(40));
        let mut fills = Vec::new();
        e.submit(9, Side::Buy, 101, 10, &mut fills).unwrap();
        assert_eq!(fills[0].resting_id, 1);
        assert_eq!(e.delete(2), Ok(50));
        assert_eq!(e.delete(2), Err(EngineError::UnknownId(2)));
        assert_eq!(
            e.cancel(1, 31),
            Err(EngineError::ExceedsRemaining {
                asked: 31,
                remaining: 30
            })
        );
    }

    #[test]
    fn replace_goes_to_the_back_of_the_queue() {
        let mut e = engine_with_asks();
        let mut fills = Vec::new();
        // Order 1 is replaced at the same price and loses its place to order 2.
        assert_eq!(e.replace(1, 10, 101, 100, &mut fills), Ok(100));
        e.submit(9, Side::Buy, 101, 10, &mut fills).unwrap();
        assert_eq!(fills[0].resting_id, 2);
        assert_eq!(e.remaining(1), None);
        assert_eq!(e.resting(10), Some((Side::Sell, 101)));
    }

    #[test]
    fn replace_into_the_spread_can_trade() {
        let mut e = engine_with_asks();
        let mut fills = Vec::new();
        e.submit(9, Side::Buy, 100, 10, &mut fills).unwrap();
        assert_eq!(e.replace(9, 11, 101, 30, &mut fills), Ok(0));
        assert_eq!(fills[0].resting_id, 1);
        assert_eq!(fills[0].qty, 30);
    }

    #[test]
    fn bad_requests_are_refused_and_leave_the_book_alone() {
        let mut e = engine_with_asks();
        let mut fills = Vec::new();
        assert_eq!(
            e.submit(1, Side::Buy, 1, 1, &mut fills),
            Err(EngineError::DuplicateId(1))
        );
        assert_eq!(
            e.submit(9, Side::Buy, 1, 0, &mut fills),
            Err(EngineError::ZeroQty)
        );
        assert_eq!(
            e.replace(1, 2, 105, 5, &mut fills),
            Err(EngineError::DuplicateId(2))
        );
        assert_eq!(
            e.replace(77, 78, 105, 5, &mut fills),
            Err(EngineError::UnknownId(77))
        );
        assert_eq!(e.book().order_count(), 3);
        e.book().audit().unwrap();
    }
}
