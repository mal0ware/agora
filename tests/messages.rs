//! One test per ITCH 5.0 message type.
//!
//! Each message is laid out by hand at the offsets the specification gives
//! (the section is named in each test), without using the crate's encoder, and
//! then parsed. A second check runs the encoder over the parsed message and
//! expects the original bytes back.

use agora::book::Side;
use agora::itch::{message_len, parse, Body, Header, Message, ParseError};

/// Builds a message field by field, checking each field's offset.
struct Wire(Vec<u8>);

impl Wire {
    /// Type byte at offset 0, then locate (1), tracking (3), timestamp (5).
    fn new(msg_type: u8, locate: u16, tracking: u16, timestamp: u64) -> Self {
        let mut w = Wire(vec![msg_type]);
        w = w.at(1, &locate.to_be_bytes());
        w = w.at(3, &tracking.to_be_bytes());
        w.at(5, &timestamp.to_be_bytes()[2..])
    }

    /// Append `bytes`, asserting they start at `offset`.
    fn at(mut self, offset: usize, bytes: &[u8]) -> Self {
        assert_eq!(self.0.len(), offset, "field does not start at its offset");
        self.0.extend_from_slice(bytes);
        self
    }

    fn done(self, len: usize) -> Vec<u8> {
        assert_eq!(self.0.len(), len, "total length");
        assert_eq!(message_len(self.0[0]), Some(len));
        self.0
    }
}

const TS: u64 = 34_200_123_456_789; // 09:30:00.123456789
const HEADER: Header = Header {
    locate: 0x1234,
    tracking: 0x0102,
    timestamp: TS,
};

fn check(bytes: Vec<u8>, body: Body) {
    let expected = Message {
        header: HEADER,
        body,
    };
    assert_eq!(parse(&bytes), Ok(expected));
    let mut encoded = Vec::new();
    expected.encode(&mut encoded);
    assert_eq!(
        encoded, bytes,
        "encoder disagrees with the hand-built bytes"
    );
    assert_eq!(expected.type_byte(), bytes[0]);
    // One byte short or long is rejected.
    let short = &bytes[..bytes.len() - 1];
    assert!(matches!(parse(short), Err(ParseError::BadLength { .. })));
    let mut long = bytes.clone();
    long.push(0);
    assert!(matches!(parse(&long), Err(ParseError::BadLength { .. })));
}

fn wire(msg_type: u8) -> Wire {
    Wire::new(msg_type, 0x1234, 0x0102, TS)
}

#[test]
fn header_bytes_are_big_endian() {
    // System Event with every header byte written out literally.
    let bytes = [
        b'S', // 0: message type
        0x12, 0x34, // 1: stock locate
        0x01, 0x02, // 3: tracking number
        0x1f, 0x1a, 0xd6, 0x35, 0xbd, 0x15, // 5: timestamp, 48 bits
        b'O', // 11: event code
    ];
    assert_eq!(0x1f1a_d635_bd15_u64, TS);
    assert_eq!(
        parse(&bytes),
        Ok(Message {
            header: HEADER,
            body: Body::SystemEvent { event: b'O' },
        })
    );
}

#[test]
fn system_event_s_section_1_1() {
    check(
        wire(b'S').at(11, b"Q").done(12),
        Body::SystemEvent { event: b'Q' },
    );
}

#[test]
fn stock_directory_r_section_1_2_1() {
    let bytes = wire(b'R')
        .at(11, b"AAPL    ")
        .at(19, b"Q")
        .at(20, b"N")
        .at(21, &100u32.to_be_bytes())
        .at(25, b"N")
        .at(26, b"C")
        .at(27, b"Z ")
        .at(29, b"P")
        .at(30, b"N")
        .at(31, b"N")
        .at(32, b"1")
        .at(33, b"N")
        .at(34, &3u32.to_be_bytes())
        .at(38, b"Y")
        .done(39);
    check(
        bytes,
        Body::StockDirectory {
            stock: *b"AAPL    ",
            market_category: b'Q',
            financial_status: b'N',
            round_lot_size: 100,
            round_lots_only: b'N',
            issue_classification: b'C',
            issue_sub_type: *b"Z ",
            authenticity: b'P',
            short_sale_threshold: b'N',
            ipo_flag: b'N',
            luld_tier: b'1',
            etp_flag: b'N',
            etp_leverage: 3,
            inverse: b'Y',
        },
    );
}

#[test]
fn stock_trading_action_h_section_1_2_2() {
    let bytes = wire(b'H')
        .at(11, b"MSFT    ")
        .at(19, b"H")
        .at(20, b" ")
        .at(21, b"LUDP")
        .done(25);
    check(
        bytes,
        Body::TradingAction {
            stock: *b"MSFT    ",
            state: b'H',
            reserved: b' ',
            reason: *b"LUDP",
        },
    );
}

#[test]
fn reg_sho_y_section_1_2_3() {
    check(
        wire(b'Y').at(11, b"GE      ").at(19, b"1").done(20),
        Body::RegSho {
            stock: *b"GE      ",
            action: b'1',
        },
    );
}

#[test]
fn market_participant_position_l_section_1_2_4() {
    let bytes = wire(b'L')
        .at(11, b"GSCO")
        .at(15, b"INTC    ")
        .at(23, b"Y")
        .at(24, b"N")
        .at(25, b"A")
        .done(26);
    check(
        bytes,
        Body::MarketParticipantPosition {
            mpid: *b"GSCO",
            stock: *b"INTC    ",
            primary_market_maker: b'Y',
            market_maker_mode: b'N',
            state: b'A',
        },
    );
}

#[test]
fn mwcb_decline_level_v_section_1_2_5_1() {
    let bytes = wire(b'V')
        .at(11, &300_012_345_678u64.to_be_bytes())
        .at(19, &280_000_000_000u64.to_be_bytes())
        .at(27, &250_000_000_001u64.to_be_bytes())
        .done(35);
    check(
        bytes,
        Body::MwcbDeclineLevel {
            level1: 300_012_345_678,
            level2: 280_000_000_000,
            level3: 250_000_000_001,
        },
    );
}

#[test]
fn mwcb_status_w_section_1_2_5_2() {
    check(
        wire(b'W').at(11, b"2").done(12),
        Body::MwcbStatus {
            breached_level: b'2',
        },
    );
}

#[test]
fn ipo_quoting_period_k_section_1_2_6() {
    let bytes = wire(b'K')
        .at(11, b"NEWCO   ")
        .at(19, &37_800u32.to_be_bytes())
        .at(23, b"A")
        .at(24, &250_000u32.to_be_bytes())
        .done(28);
    check(
        bytes,
        Body::IpoQuotingPeriod {
            stock: *b"NEWCO   ",
            release_time: 37_800,
            qualifier: b'A',
            price: 250_000,
        },
    );
}

#[test]
fn luld_auction_collar_j_section_1_2_7() {
    let bytes = wire(b'J')
        .at(11, b"TSLA    ")
        .at(19, &4_000_000u32.to_be_bytes())
        .at(23, &4_200_000u32.to_be_bytes())
        .at(27, &3_800_000u32.to_be_bytes())
        .at(31, &2u32.to_be_bytes())
        .done(35);
    check(
        bytes,
        Body::LuldAuctionCollar {
            stock: *b"TSLA    ",
            reference_price: 4_000_000,
            upper_price: 4_200_000,
            lower_price: 3_800_000,
            extension: 2,
        },
    );
}

#[test]
fn operational_halt_h_lower_section_1_2_8() {
    check(
        wire(b'h')
            .at(11, b"IBM     ")
            .at(19, b"Q")
            .at(20, b"H")
            .done(21),
        Body::OperationalHalt {
            stock: *b"IBM     ",
            market_code: b'Q',
            action: b'H',
        },
    );
}

#[test]
fn add_order_a_section_1_3_1() {
    let bytes = wire(b'A')
        .at(11, &0x0102_0304_0506_0708u64.to_be_bytes())
        .at(19, b"B")
        .at(20, &500u32.to_be_bytes())
        .at(24, b"AAPL    ")
        .at(32, &2_915_500u32.to_be_bytes())
        .done(36);
    check(
        bytes,
        Body::AddOrder {
            order_ref: 0x0102_0304_0506_0708,
            side: Side::Buy,
            shares: 500,
            stock: *b"AAPL    ",
            price: 2_915_500,
            mpid: None,
        },
    );
}

#[test]
fn add_order_with_mpid_f_section_1_3_2() {
    let bytes = wire(b'F')
        .at(11, &77u64.to_be_bytes())
        .at(19, b"S")
        .at(20, &100u32.to_be_bytes())
        .at(24, b"ZVZZT   ")
        .at(32, &100_100u32.to_be_bytes())
        .at(36, b"NITE")
        .done(40);
    check(
        bytes,
        Body::AddOrder {
            order_ref: 77,
            side: Side::Sell,
            shares: 100,
            stock: *b"ZVZZT   ",
            price: 100_100,
            mpid: Some(*b"NITE"),
        },
    );
}

#[test]
fn add_order_with_a_bad_side_is_rejected() {
    for t in *b"AF" {
        let mut w = wire(t)
            .at(11, &1u64.to_be_bytes())
            .at(19, b"X")
            .at(20, &1u32.to_be_bytes())
            .at(24, b"AAPL    ")
            .at(32, &1u32.to_be_bytes());
        if t == b'F' {
            w = w.at(36, b"NITE");
        }
        assert_eq!(parse(&w.0), Err(ParseError::BadSide(b'X')));
    }
}

#[test]
fn order_executed_e_section_1_4_1() {
    let bytes = wire(b'E')
        .at(11, &42u64.to_be_bytes())
        .at(19, &300u32.to_be_bytes())
        .at(23, &9_000_000_001u64.to_be_bytes())
        .done(31);
    check(
        bytes,
        Body::OrderExecuted {
            order_ref: 42,
            shares: 300,
            match_number: 9_000_000_001,
        },
    );
}

#[test]
fn order_executed_with_price_c_section_1_4_2() {
    let bytes = wire(b'C')
        .at(11, &42u64.to_be_bytes())
        .at(19, &300u32.to_be_bytes())
        .at(23, &17u64.to_be_bytes())
        .at(31, b"N")
        .at(32, &1_234_500u32.to_be_bytes())
        .done(36);
    check(
        bytes,
        Body::OrderExecutedWithPrice {
            order_ref: 42,
            shares: 300,
            match_number: 17,
            printable: b'N',
            price: 1_234_500,
        },
    );
}

#[test]
fn order_cancel_x_section_1_4_3() {
    check(
        wire(b'X')
            .at(11, &42u64.to_be_bytes())
            .at(19, &25u32.to_be_bytes())
            .done(23),
        Body::OrderCancel {
            order_ref: 42,
            shares: 25,
        },
    );
}

#[test]
fn order_delete_d_section_1_4_4() {
    check(
        wire(b'D').at(11, &u64::MAX.to_be_bytes()).done(19),
        Body::OrderDelete {
            order_ref: u64::MAX,
        },
    );
}

#[test]
fn order_replace_u_section_1_4_5() {
    let bytes = wire(b'U')
        .at(11, &42u64.to_be_bytes())
        .at(19, &43u64.to_be_bytes())
        .at(27, &700u32.to_be_bytes())
        .at(31, &999_900u32.to_be_bytes())
        .done(35);
    check(
        bytes,
        Body::OrderReplace {
            old_ref: 42,
            new_ref: 43,
            shares: 700,
            price: 999_900,
        },
    );
}

#[test]
fn trade_p_section_1_5_1() {
    let bytes = wire(b'P')
        .at(11, &0u64.to_be_bytes())
        .at(19, b"B")
        .at(20, &50u32.to_be_bytes())
        .at(24, b"SPY     ")
        .at(32, &3_210_000u32.to_be_bytes())
        .at(36, &123_456u64.to_be_bytes())
        .done(44);
    check(
        bytes,
        Body::Trade {
            order_ref: 0,
            side: b'B',
            shares: 50,
            stock: *b"SPY     ",
            price: 3_210_000,
            match_number: 123_456,
        },
    );
}

#[test]
fn cross_trade_q_section_1_5_2() {
    let bytes = wire(b'Q')
        .at(11, &5_000_000_000u64.to_be_bytes())
        .at(19, b"QQQ     ")
        .at(27, &2_120_000u32.to_be_bytes())
        .at(31, &99u64.to_be_bytes())
        .at(39, b"C")
        .done(40);
    check(
        bytes,
        Body::CrossTrade {
            shares: 5_000_000_000,
            stock: *b"QQQ     ",
            price: 2_120_000,
            match_number: 99,
            cross_type: b'C',
        },
    );
}

#[test]
fn broken_trade_b_section_1_5_3() {
    check(
        wire(b'B').at(11, &99u64.to_be_bytes()).done(19),
        Body::BrokenTrade { match_number: 99 },
    );
}

#[test]
fn noii_i_section_1_6() {
    let bytes = wire(b'I')
        .at(11, &10_000u64.to_be_bytes())
        .at(19, &2_500u64.to_be_bytes())
        .at(27, b"S")
        .at(28, b"NFLX    ")
        .at(36, &3_200_000u32.to_be_bytes())
        .at(40, &3_210_000u32.to_be_bytes())
        .at(44, &3_205_000u32.to_be_bytes())
        .at(48, b"O")
        .at(49, b"L")
        .done(50);
    check(
        bytes,
        Body::Noii {
            paired_shares: 10_000,
            imbalance_shares: 2_500,
            direction: b'S',
            stock: *b"NFLX    ",
            far_price: 3_200_000,
            near_price: 3_210_000,
            reference_price: 3_205_000,
            cross_type: b'O',
            variation: b'L',
        },
    );
}

#[test]
fn rpii_n_section_1_7() {
    check(
        wire(b'N').at(11, b"F       ").at(19, b"A").done(20),
        Body::Rpii {
            stock: *b"F       ",
            interest: b'A',
        },
    );
}

#[test]
fn dlcr_price_discovery_o_section_1_8() {
    let bytes = wire(b'O')
        .at(11, b"DLCR    ")
        .at(19, b"Y")
        .at(20, &160_000u32.to_be_bytes())
        .at(24, &360_000u32.to_be_bytes())
        .at(28, &250_000u32.to_be_bytes())
        .at(32, &37_800_000_000_000u64.to_be_bytes())
        .at(40, &225_000u32.to_be_bytes())
        .at(44, &275_000u32.to_be_bytes())
        .done(48);
    check(
        bytes,
        Body::DlcrPriceDiscovery {
            stock: *b"DLCR    ",
            eligibility: b'Y',
            min_price: 160_000,
            max_price: 360_000,
            near_price: 250_000,
            near_time: 37_800_000_000_000,
            lower_collar: 225_000,
            upper_collar: 275_000,
        },
    );
}

#[test]
fn empty_and_unknown_types_are_rejected() {
    assert_eq!(parse(&[]), Err(ParseError::Empty));
    assert_eq!(parse(&[b'Z'; 20]), Err(ParseError::UnknownType(b'Z')));
    assert_eq!(parse(&[0u8; 12]), Err(ParseError::UnknownType(0)));
}

#[test]
fn the_maximum_price_in_the_specification_fits() {
    // Data Types: the largest Price(4) is 200,000.0000, 0x77359400.
    assert_eq!(0x7735_9400u32, 2_000_000_000);
    let bytes = wire(b'U')
        .at(11, &1u64.to_be_bytes())
        .at(19, &2u64.to_be_bytes())
        .at(27, &1u32.to_be_bytes())
        .at(31, &[0x77, 0x35, 0x94, 0x00])
        .done(35);
    match parse(&bytes).unwrap().body {
        Body::OrderReplace { price, .. } => assert_eq!(price, 2_000_000_000),
        other => panic!("{other:?}"),
    }
}
