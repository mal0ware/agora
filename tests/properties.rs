//! Property tests.
//!
//! - The matching engine agrees with a brute-force model, is never crossed or
//!   locked, and conserves volume.
//! - The parser round-trips every message and never panics on arbitrary bytes.
//! - Replay of generated streams rebuilds the generator's books, and replay of
//!   arbitrary (mostly invalid) order messages still balances its ledger.
//! - The order reference index agrees with a hash map.
//! - A book driven with stale handles refuses them and stays sound.

use std::collections::HashMap;

use agora::book::{Book, BookError, Handle, Side};
use agora::engine::{Engine, Fill};
use agora::index::{HashIndex, OrderIndex, RefIndex, DIRECT_LIMIT};
use agora::itch::{message_len, parse, Body, FrameReader, Header, Message};
use agora::replay::{Replay, Resting};
use agora::synth::{generate, SynthConfig};
use proptest::prelude::*;

// ---------------------------------------------------------------- engine --

#[derive(Debug, Clone)]
enum Op {
    Submit { buy: bool, price: u32, qty: u32 },
    Cancel { pick: usize, qty: u32 },
    Delete { pick: usize },
    Replace { pick: usize, price: u32, qty: u32 },
}

fn op() -> impl Strategy<Value = Op> {
    let price = 95u32..=105;
    let qty = 1u32..=60;
    prop_oneof![
        5 => (any::<bool>(), price.clone(), qty.clone())
            .prop_map(|(buy, price, qty)| Op::Submit { buy, price, qty }),
        1 => (any::<usize>(), qty.clone()).prop_map(|(pick, qty)| Op::Cancel { pick, qty }),
        1 => any::<usize>().prop_map(|pick| Op::Delete { pick }),
        1 => (any::<usize>(), price, qty)
            .prop_map(|(pick, price, qty)| Op::Replace { pick, price, qty }),
    ]
}

/// The slowest possible correct book: a flat list searched in full each time.
#[derive(Default)]
struct Model {
    /// (id, side, price, qty, arrival sequence)
    orders: Vec<(u64, Side, u32, u32, u64)>,
    seq: u64,
}

impl Model {
    fn submit(&mut self, id: u64, side: Side, price: u32, mut qty: u32) -> Vec<Fill> {
        let mut fills = Vec::new();
        while qty > 0 {
            let best = self
                .orders
                .iter()
                .enumerate()
                .filter(|(_, o)| o.1 != side)
                .filter(|(_, o)| match side {
                    Side::Buy => o.2 <= price,
                    Side::Sell => o.2 >= price,
                })
                .min_by_key(|(_, o)| match side {
                    Side::Buy => (i64::from(o.2), o.4),
                    Side::Sell => (-i64::from(o.2), o.4),
                })
                .map(|(i, _)| i);
            let Some(i) = best else { break };
            let traded = qty.min(self.orders[i].3);
            self.orders[i].3 -= traded;
            qty -= traded;
            fills.push(Fill {
                resting_id: self.orders[i].0,
                price: self.orders[i].2,
                qty: traded,
                resting_remaining: self.orders[i].3,
            });
            if self.orders[i].3 == 0 {
                self.orders.remove(i);
            }
        }
        if qty > 0 {
            self.seq += 1;
            self.orders.push((id, side, price, qty, self.seq));
        }
        fills
    }

    fn find(&self, id: u64) -> usize {
        self.orders.iter().position(|o| o.0 == id).unwrap()
    }

    /// Orders on one side in priority order, as (id, price, qty).
    fn side(&self, side: Side) -> Vec<(u64, u32, u32)> {
        let mut v: Vec<_> = self.orders.iter().filter(|o| o.1 == side).collect();
        v.sort_by_key(|o| match side {
            Side::Buy => (-i64::from(o.2), o.4),
            Side::Sell => (i64::from(o.2), o.4),
        });
        v.into_iter().map(|o| (o.0, o.2, o.3)).collect()
    }
}

fn engine_side(engine: &Engine, side: Side) -> Vec<(u64, u32, u32)> {
    let book = engine.book();
    book.levels(side)
        .flat_map(|l| book.queue(side, l.price).map(|o| (o.id, o.price, o.qty)))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn engine_matches_the_model_and_keeps_its_invariants(ops in prop::collection::vec(op(), 1..200)) {
        let mut engine = Engine::new();
        let mut model = Model::default();
        let mut live: Vec<u64> = Vec::new();
        let mut next_id = 0u64;
        let mut fills = Vec::new();
        // Ledger: everything submitted must end up traded (twice: once per
        // side of each trade), removed, or resting.
        let (mut submitted, mut traded, mut removed) = (0u64, 0u64, 0u64);

        for op in ops {
            fills.clear();
            live.retain(|id| engine.remaining(*id).is_some());
            match op {
                Op::Submit { buy, price, qty } => {
                    let side = if buy { Side::Buy } else { Side::Sell };
                    next_id += 1;
                    let rested = engine.submit(next_id, side, price, qty, &mut fills).unwrap();
                    let expected = model.submit(next_id, side, price, qty);
                    prop_assert_eq!(&fills, &expected);
                    submitted += u64::from(qty);
                    let filled: u64 = fills.iter().map(|f| u64::from(f.qty)).sum();
                    prop_assert_eq!(u64::from(rested) + filled, u64::from(qty));
                    if rested > 0 {
                        live.push(next_id);
                    }
                }
                Op::Cancel { pick, qty } => {
                    if live.is_empty() { continue; }
                    let id = live[pick % live.len()];
                    let have = engine.remaining(id).unwrap();
                    let qty = qty.min(have);
                    let left = engine.cancel(id, qty).unwrap();
                    prop_assert_eq!(left, have - qty);
                    let i = model.find(id);
                    model.orders[i].3 -= qty;
                    if model.orders[i].3 == 0 {
                        model.orders.remove(i);
                    }
                    removed += u64::from(qty);
                }
                Op::Delete { pick } => {
                    if live.is_empty() { continue; }
                    let id = live[pick % live.len()];
                    let had = engine.delete(id).unwrap();
                    let i = model.find(id);
                    prop_assert_eq!(had, model.orders[i].3);
                    model.orders.remove(i);
                    removed += u64::from(had);
                }
                Op::Replace { pick, price, qty } => {
                    if live.is_empty() { continue; }
                    let id = live[pick % live.len()];
                    let had = engine.remaining(id).unwrap();
                    let (side, _) = engine.resting(id).unwrap();
                    next_id += 1;
                    let rested = engine.replace(id, next_id, price, qty, &mut fills).unwrap();
                    let i = model.find(id);
                    model.orders.remove(i);
                    let expected = model.submit(next_id, side, price, qty);
                    prop_assert_eq!(&fills, &expected);
                    removed += u64::from(had);
                    submitted += u64::from(qty);
                    if rested > 0 {
                        live.push(next_id);
                    }
                }
            }
            traded += fills.iter().map(|f| u64::from(f.qty)).sum::<u64>();

            let book = engine.book();
            prop_assert!(!book.is_crossed(), "crossed");
            prop_assert!(!book.is_locked(), "locked");
            if let Err(e) = book.audit() {
                prop_assert!(false, "audit: {}", e);
            }
            prop_assert_eq!(submitted, 2 * traded + removed + book.resting_qty());
        }
        prop_assert_eq!(engine_side(&engine, Side::Buy), model.side(Side::Buy));
        prop_assert_eq!(engine_side(&engine, Side::Sell), model.side(Side::Sell));
    }

    #[test]
    fn fills_never_trade_through_the_limit(ops in prop::collection::vec(op(), 1..100)) {
        let mut engine = Engine::new();
        let mut fills = Vec::new();
        let mut id = 0;
        for op in ops {
            if let Op::Submit { buy, price, qty } = op {
                id += 1;
                fills.clear();
                let side = if buy { Side::Buy } else { Side::Sell };
                let best_before = engine.book().best(side.opposite()).map(|l| l.price);
                engine.submit(id, side, price, qty, &mut fills).unwrap();
                for (i, f) in fills.iter().enumerate() {
                    match side {
                        Side::Buy => prop_assert!(f.price <= price),
                        Side::Sell => prop_assert!(f.price >= price),
                    }
                    if i == 0 {
                        prop_assert_eq!(Some(f.price), best_before);
                    } else {
                        // Prices only get worse for the taker as it walks the book.
                        match side {
                            Side::Buy => prop_assert!(f.price >= fills[i - 1].price),
                            Side::Sell => prop_assert!(f.price <= fills[i - 1].price),
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- parser --

fn header() -> impl Strategy<Value = Header> {
    (any::<u16>(), any::<u16>(), 0u64..(1 << 48)).prop_map(|(locate, tracking, timestamp)| Header {
        locate,
        tracking,
        timestamp,
    })
}

fn side() -> impl Strategy<Value = Side> {
    prop_oneof![Just(Side::Buy), Just(Side::Sell)]
}

fn body() -> impl Strategy<Value = Body> {
    let stock = any::<[u8; 8]>();
    prop_oneof![
        any::<u8>().prop_map(|event| Body::SystemEvent { event }),
        (stock, any::<[u8; 12]>(), any::<u32>(), any::<u32>()).prop_map(|(stock, b, lot, lev)| {
            Body::StockDirectory {
                stock,
                market_category: b[0],
                financial_status: b[1],
                round_lot_size: lot,
                round_lots_only: b[2],
                issue_classification: b[3],
                issue_sub_type: [b[4], b[5]],
                authenticity: b[6],
                short_sale_threshold: b[7],
                ipo_flag: b[8],
                luld_tier: b[9],
                etp_flag: b[10],
                etp_leverage: lev,
                inverse: b[11],
            }
        }),
        (stock, any::<u8>(), any::<u8>(), any::<[u8; 4]>()).prop_map(
            |(stock, state, reserved, reason)| Body::TradingAction {
                stock,
                state,
                reserved,
                reason,
            }
        ),
        (stock, any::<u8>()).prop_map(|(stock, action)| Body::RegSho { stock, action }),
        (any::<[u8; 4]>(), stock, any::<[u8; 3]>()).prop_map(|(mpid, stock, b)| {
            Body::MarketParticipantPosition {
                mpid,
                stock,
                primary_market_maker: b[0],
                market_maker_mode: b[1],
                state: b[2],
            }
        }),
        any::<[u64; 3]>().prop_map(|l| Body::MwcbDeclineLevel {
            level1: l[0],
            level2: l[1],
            level3: l[2],
        }),
        any::<u8>().prop_map(|breached_level| Body::MwcbStatus { breached_level }),
        (stock, any::<u32>(), any::<u8>(), any::<u32>()).prop_map(
            |(stock, release_time, qualifier, price)| Body::IpoQuotingPeriod {
                stock,
                release_time,
                qualifier,
                price,
            }
        ),
        (stock, any::<[u32; 4]>()).prop_map(|(stock, p)| Body::LuldAuctionCollar {
            stock,
            reference_price: p[0],
            upper_price: p[1],
            lower_price: p[2],
            extension: p[3],
        }),
        (stock, any::<u8>(), any::<u8>()).prop_map(|(stock, market_code, action)| {
            Body::OperationalHalt {
                stock,
                market_code,
                action,
            }
        }),
        (
            any::<u64>(),
            side(),
            any::<u32>(),
            stock,
            any::<u32>(),
            any::<Option<[u8; 4]>>()
        )
            .prop_map(|(order_ref, side, shares, stock, price, mpid)| {
                Body::AddOrder {
                    order_ref,
                    side,
                    shares,
                    stock,
                    price,
                    mpid,
                }
            }),
        (any::<u64>(), any::<u32>(), any::<u64>()).prop_map(|(order_ref, shares, match_number)| {
            Body::OrderExecuted {
                order_ref,
                shares,
                match_number,
            }
        }),
        (
            any::<u64>(),
            any::<u32>(),
            any::<u64>(),
            any::<u8>(),
            any::<u32>()
        )
            .prop_map(|(order_ref, shares, match_number, printable, price)| {
                Body::OrderExecutedWithPrice {
                    order_ref,
                    shares,
                    match_number,
                    printable,
                    price,
                }
            }),
        (any::<u64>(), any::<u32>())
            .prop_map(|(order_ref, shares)| Body::OrderCancel { order_ref, shares }),
        any::<u64>().prop_map(|order_ref| Body::OrderDelete { order_ref }),
        (any::<u64>(), any::<u64>(), any::<u32>(), any::<u32>()).prop_map(
            |(old_ref, new_ref, shares, price)| Body::OrderReplace {
                old_ref,
                new_ref,
                shares,
                price,
            }
        ),
        (
            any::<u64>(),
            any::<u8>(),
            any::<u32>(),
            stock,
            any::<u32>(),
            any::<u64>()
        )
            .prop_map(|(order_ref, side, shares, stock, price, match_number)| {
                Body::Trade {
                    order_ref,
                    side,
                    shares,
                    stock,
                    price,
                    match_number,
                }
            }),
        (any::<u64>(), stock, any::<u32>(), any::<u64>(), any::<u8>()).prop_map(
            |(shares, stock, price, match_number, cross_type)| Body::CrossTrade {
                shares,
                stock,
                price,
                match_number,
                cross_type,
            }
        ),
        any::<u64>().prop_map(|match_number| Body::BrokenTrade { match_number }),
        (
            any::<[u64; 2]>(),
            stock,
            any::<[u32; 3]>(),
            any::<[u8; 3]>()
        )
            .prop_map(|(q, stock, p, b)| Body::Noii {
                paired_shares: q[0],
                imbalance_shares: q[1],
                direction: b[0],
                stock,
                far_price: p[0],
                near_price: p[1],
                reference_price: p[2],
                cross_type: b[1],
                variation: b[2],
            }),
        (stock, any::<u8>()).prop_map(|(stock, interest)| Body::Rpii { stock, interest }),
        (stock, any::<u8>(), any::<[u32; 5]>(), any::<u64>()).prop_map(
            |(stock, eligibility, p, near_time)| Body::DlcrPriceDiscovery {
                stock,
                eligibility,
                min_price: p[0],
                max_price: p[1],
                near_price: p[2],
                near_time,
                lower_collar: p[3],
                upper_collar: p[4],
            }
        ),
    ]
}

fn message() -> impl Strategy<Value = Message> {
    (header(), body()).prop_map(|(header, body)| Message { header, body })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn encode_then_parse_is_the_identity(m in message()) {
        let mut bytes = Vec::new();
        m.encode(&mut bytes);
        prop_assert_eq!(message_len(bytes[0]), Some(bytes.len()));
        prop_assert_eq!(parse(&bytes), Ok(m));
    }

    #[test]
    fn parse_then_encode_is_the_identity(mut bytes in prop::collection::vec(any::<u8>(), 50)) {
        // Any bytes of the right length with a valid type (and side) parse,
        // and encoding the result gives the same bytes back: no field is
        // dropped, moved or reinterpreted.
        let types = b"SRHYLVWKJhAFECXDUPQBINO";
        bytes[0] = types[usize::from(bytes[0]) % types.len()];
        bytes.truncate(message_len(bytes[0]).unwrap());
        if bytes[0] == b'A' || bytes[0] == b'F' {
            bytes[19] = if bytes[19] % 2 == 0 { b'B' } else { b'S' };
        }
        let m = parse(&bytes).unwrap();
        let mut again = Vec::new();
        m.encode(&mut again);
        prop_assert_eq!(again, bytes);
    }

    #[test]
    fn parse_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        let _ = parse(&bytes);
    }

    #[test]
    fn frame_reader_is_independent_of_chunking(
        messages in prop::collection::vec(message(), 0..40),
        chunk in 1usize..97,
    ) {
        struct Chunked<'a>(&'a [u8], usize);
        impl std::io::Read for Chunked<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.1.min(buf.len()).min(self.0.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let mut bytes = Vec::new();
        for m in &messages {
            m.encode_framed(&mut bytes);
        }
        let mut reader = FrameReader::new(Chunked(&bytes, chunk));
        let mut got = Vec::new();
        while let Some(frame) = reader.next_frame().unwrap() {
            got.push(parse(frame).unwrap());
        }
        prop_assert_eq!(got, messages);
        prop_assert_eq!(reader.offset(), bytes.len() as u64);
    }
}

// ---------------------------------------------------------------- replay --

fn replay_bytes(bytes: &[u8], check: bool) -> Replay {
    let mut replay = Replay::new(check);
    let mut reader = FrameReader::new(bytes);
    while let Some(frame) = reader.next_frame().unwrap() {
        replay.apply(&parse(frame).unwrap());
    }
    replay
}

fn order_message() -> impl Strategy<Value = Message> {
    // Small ranges so that references collide, repeat and go missing.
    let r = 1u64..40;
    let shares = 0u32..300;
    let price = 90u32..110;
    let body = prop_oneof![
        4 => (r.clone(), side(), shares.clone(), price.clone()).prop_map(|(order_ref, side, shares, price)| {
            Body::AddOrder { order_ref, side, shares, stock: *b"TEST    ", price, mpid: None }
        }),
        2 => (r.clone(), shares.clone()).prop_map(|(order_ref, shares)| {
            Body::OrderExecuted { order_ref, shares, match_number: 0 }
        }),
        1 => (r.clone(), shares.clone(), any::<bool>()).prop_map(|(order_ref, shares, p)| {
            Body::OrderExecutedWithPrice {
                order_ref, shares, match_number: 0,
                printable: if p { b'Y' } else { b'N' }, price: 100,
            }
        }),
        2 => (r.clone(), shares.clone()).prop_map(|(order_ref, shares)| {
            Body::OrderCancel { order_ref, shares }
        }),
        2 => r.clone().prop_map(|order_ref| Body::OrderDelete { order_ref }),
        2 => (r.clone(), r, shares, price).prop_map(|(old_ref, new_ref, shares, price)| {
            Body::OrderReplace { old_ref, new_ref, shares, price }
        }),
    ];
    (1u16..4, 0u64..1_000_000, body).prop_map(|(locate, timestamp, body)| Message {
        header: Header {
            locate,
            tracking: 0,
            timestamp,
        },
        body,
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn replay_rebuilds_the_generators_books(seed in any::<u64>(), messages in 200u64..4000, symbols in 1u16..6) {
        let synth = generate(&SynthConfig { seed, messages, symbols });
        let mut checked = replay_bytes(&synth.bytes, true);
        let unchecked = replay_bytes(&synth.bytes, false);
        let audit = checked.finish();
        let s = &checked.stats;
        prop_assert_eq!(s.messages, synth.messages);
        prop_assert_eq!(&s.counts[..], &synth.counts[..]);
        prop_assert_eq!(audit.digest, synth.digest);
        prop_assert_eq!(unchecked.digest(), synth.digest);
        prop_assert_eq!(audit.resting_orders, synth.resting_orders);
        prop_assert_eq!(audit.resting_shares, synth.resting_shares);
        prop_assert_eq!(s.shares_executed, synth.traded_shares);
        prop_assert!(audit.conserved());
        // The generator is a matching engine, so its feed never crosses or
        // locks a book and every execution takes the front of the queue.
        prop_assert_eq!(s.crossed_updates, 0);
        prop_assert_eq!(s.locked_updates, 0);
        prop_assert_eq!(s.exec_e_at_front, s.exec_e);
        prop_assert_eq!(s.exec_c_at_front, s.exec_c);
        prop_assert_eq!(
            s.unknown_ref + s.duplicate_ref + s.over_reduce + s.locate_mismatch + s.zero_share_add,
            0
        );
        prop_assert_eq!(s.time_reversals, 0);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn replay_survives_arbitrary_order_messages(messages in prop::collection::vec(order_message(), 0..300)) {
        let mut replay = Replay::new(true);
        for m in &messages {
            replay.apply(m);
        }
        let audit = replay.finish();
        prop_assert!(audit.structure_errors.is_empty(), "{:?}", audit.structure_errors);
        prop_assert_eq!(audit.ledger_gap, 0);
        prop_assert!(audit.conserved());
        prop_assert_eq!(replay.live_orders() as u64, audit.resting_orders);
        prop_assert_eq!(replay.stats.messages, messages.len() as u64);
    }
}

// ----------------------------------------------------------------- index --

fn index_key() -> impl Strategy<Value = u64> {
    // Keys that share pages, keys on page edges, and keys past the page
    // table's limit, drawn from small sets so that they collide.
    prop_oneof![
        4 => 0u64..64,
        2 => (0u64..4).prop_map(|p| p * 4096 + 4095),
        2 => (0u64..4).prop_map(|p| p * 4096),
        1 => (0u64..8).prop_map(|k| DIRECT_LIMIT - 4 + k),
        1 => (0u64..4).prop_map(|k| u64::MAX - k),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn ref_index_agrees_with_a_hash_map(ops in prop::collection::vec((0u8..3, index_key(), any::<u32>()), 0..400)) {
        let mut index: RefIndex<u32> = RefIndex::new();
        let mut hashed: HashIndex<u32> = HashIndex::default();
        let mut model: HashMap<u64, u32> = HashMap::new();
        for (op, key, value) in ops {
            match op {
                0 => {
                    let was = model.insert(key, value);
                    prop_assert_eq!(index.insert(key, value), was);
                    prop_assert_eq!(OrderIndex::insert(&mut hashed, key, value), was);
                }
                1 => {
                    let was = model.remove(&key);
                    prop_assert_eq!(index.remove(key), was);
                    prop_assert_eq!(OrderIndex::remove(&mut hashed, key), was);
                }
                _ => {
                    index.prefetch(key);
                    let is = model.get(&key).copied();
                    prop_assert_eq!(index.get(key), is);
                    prop_assert_eq!(OrderIndex::get(&hashed, key), is);
                    prop_assert_eq!(OrderIndex::contains(&hashed, key), is.is_some());
                }
            }
            prop_assert_eq!(index.len(), model.len());
            prop_assert_eq!(OrderIndex::len(&hashed), model.len());
        }
        for (key, value) in &model {
            prop_assert_eq!(index.get(*key), Some(*value));
        }
        for key in model.keys() {
            index.remove(*key);
        }
        prop_assert!(index.is_empty());
        prop_assert_eq!(index.pages_in_use(), 0);
    }
}

// ------------------------------------------------------------------ book --

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Drive a book with handles that are sometimes stale. Every call either
    /// does what a list of live orders says it should or is refused, and the
    /// book passes its audit after each one.
    #[test]
    fn stale_handles_never_corrupt_a_book(ops in prop::collection::vec((0u8..4, any::<usize>(), 0u32..40, 95u32..100), 1..300)) {
        let mut book = Book::new();
        // Every handle ever issued, with what its order has left (0 = gone).
        let mut issued: Vec<(Handle, u32)> = Vec::new();
        let mut next_id = 0u64;
        for (op, pick, qty, price) in ops {
            if op == 0 || issued.is_empty() {
                next_id += 1;
                let side = if price < 97 { Side::Buy } else { Side::Sell };
                match book.add(next_id, side, price, qty) {
                    Ok(h) => {
                        prop_assert!(qty > 0);
                        issued.push((h, qty));
                    }
                    Err(e) => {
                        prop_assert_eq!(e, BookError::ZeroQuantity);
                        prop_assert_eq!(qty, 0);
                    }
                }
            } else {
                let i = pick % issued.len();
                let (h, left) = issued[i];
                let result = if op == 1 { book.reduce(h, qty) } else { book.remove(h) };
                if left == 0 {
                    prop_assert_eq!(result, Err(BookError::StaleHandle));
                    prop_assert_eq!(book.get(h), None);
                } else if op == 1 && qty > left {
                    prop_assert_eq!(result, Err(BookError::ExceedsRemaining { asked: qty, remaining: left }));
                } else if op == 1 {
                    prop_assert_eq!(result, Ok(left - qty));
                    issued[i].1 = left - qty;
                } else {
                    prop_assert_eq!(result, Ok(left));
                    issued[i].1 = 0;
                }
            }
            prop_assert!(book.audit().is_ok(), "{:?}", book.audit());
            let live: u64 = issued.iter().map(|(_, q)| u64::from(*q)).sum();
            prop_assert_eq!(book.resting_qty(), live);
            prop_assert_eq!(book.order_count(), issued.iter().filter(|(_, q)| *q > 0).count());
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// The prefetch hints read and never write: a replay that issues them a
    /// batch ahead ends with the same books and the same counters.
    #[test]
    fn prefetch_hints_change_nothing(seed in any::<u64>(), messages in 200u64..3000, batch in 1usize..40) {
        let synth = generate(&SynthConfig { seed, messages, symbols: 3 });
        let plain = replay_bytes(&synth.bytes, true);
        let mut parsed = Vec::new();
        let mut reader = FrameReader::new(&synth.bytes[..]);
        while let Some(frame) = reader.next_frame().unwrap() {
            parsed.push(parse(frame).unwrap());
        }
        let mut hinted = Replay::new(true);
        for chunk in parsed.chunks(batch) {
            for m in chunk {
                hinted.prefetch_index(m);
            }
            for m in chunk {
                hinted.prefetch_order(m);
            }
            for m in chunk {
                hinted.prefetch_links(m);
            }
            for m in chunk {
                hinted.apply(m);
            }
        }
        // The first version's hash map index builds the same books too.
        let mut hashed: Replay<HashIndex<Resting>> = Replay::with_index(true);
        for m in &parsed {
            hashed.apply(m);
        }
        prop_assert_eq!(hashed.digest(), plain.digest());
        prop_assert_eq!(&hashed.stats, &plain.stats);
        prop_assert_eq!(hinted.digest(), plain.digest());
        prop_assert_eq!(hinted.digest(), synth.digest);
        prop_assert_eq!(&hinted.stats, &plain.stats);
    }
}
