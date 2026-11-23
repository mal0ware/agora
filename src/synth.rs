//! A seeded generator of valid ITCH 5.0 streams.
//!
//! The real sample days are too large to commit and carry no stated licence,
//! so the tests that must run everywhere use streams made here. The generator
//! drives one [`Engine`] per symbol with random orders and writes what an
//! exchange would publish: executions against resting orders first, then the
//! add for whatever is left. Replaying the stream must therefore rebuild
//! exactly the books the engines hold, which is the end-to-end test.
//!
//! The same seed always gives the same bytes.

use crate::book::{digest_books, Side};
use crate::engine::{Engine, Fill};
use crate::itch::{stock, Body, Header, Message, Stock};

/// One price tick in the generator: one cent in Price(4) units.
const TICK: u32 = 100;

/// SplitMix64. Small, fast, and fixed here so streams never change.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// Seed the generator.
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    /// Next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A value in `0..n`. `n` must be positive. The bias is immaterial here.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// What to generate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SynthConfig {
    /// Seed.
    pub seed: u64,
    /// Number of messages to aim for. The stream ends within a few messages
    /// of this; the exact count is in [`Synth::messages`].
    pub messages: u64,
    /// Number of symbols, each with its own book.
    pub symbols: u16,
}

/// A generated stream and what the generator knows about it.
#[derive(Debug, Clone)]
pub struct Synth {
    /// The stream, each message prefixed by its two-byte length.
    pub bytes: Vec<u8>,
    /// Messages in the stream.
    pub messages: u64,
    /// Messages by type byte.
    pub counts: [u64; 256],
    /// Digest of the generator's own books at the end.
    pub digest: u64,
    /// Orders resting at the end.
    pub resting_orders: u64,
    /// Shares resting at the end.
    pub resting_shares: u64,
    /// Shares traded by the engines.
    pub traded_shares: u64,
}

struct Gen {
    rng: Rng,
    out: Vec<u8>,
    ts: u64,
    tracking: u16,
    messages: u64,
    counts: [u64; 256],
    next_ref: u64,
    next_match: u64,
    traded: u64,
}

impl Gen {
    fn emit(&mut self, locate: u16, body: Body) {
        self.ts += 1 + self.rng.below(50_000);
        self.tracking = self.tracking.wrapping_add(1);
        let m = Message {
            header: Header {
                locate,
                tracking: self.tracking,
                timestamp: self.ts,
            },
            body,
        };
        self.counts[usize::from(m.type_byte())] += 1;
        self.messages += 1;
        m.encode_framed(&mut self.out);
    }

    fn emit_fills(&mut self, locate: u16, fills: &[Fill]) {
        for fill in fills {
            self.next_match += 1;
            self.traded += u64::from(fill.qty);
            let body = if self.rng.below(16) == 0 {
                Body::OrderExecutedWithPrice {
                    order_ref: fill.resting_id,
                    shares: fill.qty,
                    match_number: self.next_match,
                    printable: if self.rng.below(4) == 0 { b'N' } else { b'Y' },
                    price: fill.price,
                }
            } else {
                Body::OrderExecuted {
                    order_ref: fill.resting_id,
                    shares: fill.qty,
                    match_number: self.next_match,
                }
            };
            self.emit(locate, body);
        }
    }
}

fn symbol(i: u16) -> Stock {
    stock(&format!("SYN{i:03}"))
}

/// Generate a stream.
pub fn generate(cfg: &SynthConfig) -> Synth {
    assert!(cfg.symbols > 0, "need at least one symbol");
    let n = usize::from(cfg.symbols);
    let mut g = Gen {
        rng: Rng::new(cfg.seed),
        out: Vec::with_capacity(cfg.messages as usize * 34),
        // 04:00:00, when Nasdaq system hours start.
        ts: 14_400_000_000_000,
        tracking: 0,
        messages: 0,
        counts: [0; 256],
        next_ref: 1,
        next_match: 0,
        traded: 0,
    };
    let mut engines: Vec<Engine> = (0..n).map(|_| Engine::new()).collect();
    let mut live: Vec<Vec<u64>> = vec![Vec::new(); n];
    let mut mids: Vec<u32> = (0..n).map(|i| 100_000 + 37_500 * i as u32).collect();
    let mut fills: Vec<Fill> = Vec::new();

    // Start of day: one of every administrative message type.
    g.emit(0, Body::SystemEvent { event: b'O' });
    for i in 0..cfg.symbols {
        let locate = i + 1;
        let stock = symbol(locate);
        g.emit(
            locate,
            Body::StockDirectory {
                stock,
                market_category: b'Q',
                financial_status: b'N',
                round_lot_size: 100,
                round_lots_only: b'N',
                issue_classification: b'C',
                issue_sub_type: *b"Z ",
                authenticity: b'T',
                short_sale_threshold: b'N',
                ipo_flag: b'N',
                luld_tier: b'1',
                etp_flag: b'N',
                etp_leverage: 0,
                inverse: b'N',
            },
        );
        g.emit(
            locate,
            Body::TradingAction {
                stock,
                state: b'T',
                reserved: b' ',
                reason: *b"    ",
            },
        );
        g.emit(
            locate,
            Body::RegSho {
                stock,
                action: b'0',
            },
        );
        g.emit(
            locate,
            Body::MarketParticipantPosition {
                mpid: *b"SYNT",
                stock,
                primary_market_maker: b'Y',
                market_maker_mode: b'N',
                state: b'A',
            },
        );
    }
    let first = symbol(1);
    g.emit(
        0,
        Body::MwcbDeclineLevel {
            level1: 300_000_000_000,
            level2: 280_000_000_000,
            level3: 250_000_000_000,
        },
    );
    g.emit(
        1,
        Body::IpoQuotingPeriod {
            stock: first,
            release_time: 34_200,
            qualifier: b'A',
            price: 100_000,
        },
    );
    g.emit(
        1,
        Body::LuldAuctionCollar {
            stock: first,
            reference_price: 100_000,
            upper_price: 105_000,
            lower_price: 95_000,
            extension: 0,
        },
    );
    g.emit(
        1,
        Body::OperationalHalt {
            stock: first,
            market_code: b'Q',
            action: b'T',
        },
    );
    g.emit(
        1,
        Body::Rpii {
            stock: first,
            interest: b'N',
        },
    );
    g.emit(
        1,
        Body::DlcrPriceDiscovery {
            stock: first,
            eligibility: b'N',
            min_price: 80_000,
            max_price: 180_000,
            near_price: 100_000,
            near_time: 34_200_000_000_000,
            lower_collar: 90_000,
            upper_collar: 110_000,
        },
    );
    g.emit(
        0,
        Body::MwcbStatus {
            breached_level: b'1',
        },
    );
    for event in *b"SQM" {
        g.emit(0, Body::SystemEvent { event });
    }

    // Order flow.
    let trailer = 2 * u64::from(cfg.symbols) + 3;
    while g.messages + trailer < cfg.messages {
        let s = g.rng.below(n as u64) as usize;
        let locate = s as u16 + 1;
        if g.rng.below(16) == 0 {
            let up = g.rng.below(2) == 0;
            mids[s] = if up {
                mids[s] + TICK
            } else {
                (mids[s] - TICK).max(20 * TICK)
            };
        }
        let mid = mids[s];
        let roll = g.rng.below(100);
        if roll < 50 {
            let side = if g.rng.below(2) == 0 {
                Side::Buy
            } else {
                Side::Sell
            };
            let aggressive = g.rng.below(100) < 12;
            let ticks = if aggressive {
                g.rng.below(4) as u32
            } else {
                1 + g.rng.below(10) as u32
            };
            let price = match (side, aggressive) {
                (Side::Buy, false) | (Side::Sell, true) => mid - ticks * TICK,
                (Side::Sell, false) | (Side::Buy, true) => mid + ticks * TICK,
            };
            let qty = if g.rng.below(7) == 0 {
                1 + g.rng.below(99) as u32
            } else {
                100 * (1 + g.rng.below(10) as u32)
            };
            let id = g.next_ref;
            g.next_ref += 1;
            fills.clear();
            let rested = engines[s]
                .submit(id, side, price, qty, &mut fills)
                .expect("fresh reference and positive quantity");
            g.emit_fills(locate, &fills);
            if rested > 0 {
                let mpid = (g.rng.below(8) == 0).then_some(*b"SYNT");
                g.emit(
                    locate,
                    Body::AddOrder {
                        order_ref: id,
                        side,
                        shares: rested,
                        stock: symbol(locate),
                        price,
                        mpid,
                    },
                );
                live[s].push(id);
            }
            continue;
        }
        if roll >= 97 {
            g.next_match += 1;
            let shares = 100 * (1 + g.rng.below(5) as u32);
            g.emit(
                locate,
                Body::Trade {
                    order_ref: 0,
                    side: b'B',
                    shares,
                    stock: symbol(locate),
                    price: mid,
                    match_number: g.next_match,
                },
            );
            continue;
        }
        // The rest act on a resting order. The list is pruned lazily.
        if live[s].is_empty() {
            continue;
        }
        let i = g.rng.below(live[s].len() as u64) as usize;
        let id = live[s][i];
        let Some(remaining) = engines[s].remaining(id) else {
            live[s].swap_remove(i);
            continue;
        };
        if roll < 62 {
            if remaining > 1 {
                let shares = 1 + g.rng.below(u64::from(remaining) - 1) as u32;
                engines[s].cancel(id, shares).expect("order rests");
                g.emit(
                    locate,
                    Body::OrderCancel {
                        order_ref: id,
                        shares,
                    },
                );
            }
        } else if roll < 85 {
            engines[s].delete(id).expect("order rests");
            live[s].swap_remove(i);
            g.emit(locate, Body::OrderDelete { order_ref: id });
        } else {
            let (side, _) = engines[s].resting(id).expect("order rests");
            let ticks = 1 + g.rng.below(10) as u32;
            // A replace on the feed never trades, so keep the new price on
            // its own side of the opposite best.
            let price = match side {
                Side::Buy => {
                    let p = mid - ticks * TICK;
                    match engines[s].book().best_ask() {
                        Some(ask) => p.min(ask - TICK),
                        None => p,
                    }
                }
                Side::Sell => {
                    let p = mid + ticks * TICK;
                    match engines[s].book().best_bid() {
                        Some(bid) => p.max(bid + TICK),
                        None => p,
                    }
                }
            };
            let shares = 100 * (1 + g.rng.below(10) as u32);
            let new_id = g.next_ref;
            g.next_ref += 1;
            fills.clear();
            engines[s]
                .replace(id, new_id, price, shares, &mut fills)
                .expect("order rests and the new reference is fresh");
            assert!(fills.is_empty(), "a generated replace must not trade");
            live[s][i] = new_id;
            g.emit(
                locate,
                Body::OrderReplace {
                    old_ref: id,
                    new_ref: new_id,
                    shares,
                    price,
                },
            );
        }
    }

    // End of day.
    for i in 0..cfg.symbols {
        let locate = i + 1;
        let stock = symbol(locate);
        let mid = mids[usize::from(i)];
        g.emit(
            locate,
            Body::Noii {
                paired_shares: 1_000,
                imbalance_shares: 200,
                direction: b'B',
                stock,
                far_price: mid,
                near_price: mid,
                reference_price: mid,
                cross_type: b'C',
                variation: b'L',
            },
        );
        g.next_match += 1;
        g.emit(
            locate,
            Body::CrossTrade {
                shares: 1_000,
                stock,
                price: mid,
                match_number: g.next_match,
                cross_type: b'C',
            },
        );
    }
    g.emit(0, Body::BrokenTrade { match_number: 1 });
    g.emit(0, Body::SystemEvent { event: b'E' });
    g.emit(0, Body::SystemEvent { event: b'C' });

    let digest = digest_books(
        engines
            .iter()
            .enumerate()
            .map(|(i, e)| (i as u64 + 1, e.book())),
    );
    Synth {
        bytes: g.out,
        messages: g.messages,
        counts: g.counts,
        digest,
        resting_orders: engines.iter().map(|e| e.book().order_count() as u64).sum(),
        resting_shares: engines.iter().map(|e| e.book().resting_qty()).sum(),
        traded_shares: g.traded,
    }
}
