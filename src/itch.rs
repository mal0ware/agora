//! Nasdaq TotalView-ITCH 5.0 messages: parse, encode and read from a file.
//!
//! Field offsets and lengths follow the Nasdaq TotalView-ITCH 5.0
//! specification; the section for each message is in [`TYPES`] and the full
//! tables are in `PAPER.md`. All integers are big-endian and unsigned. Alpha
//! fields are kept as raw ASCII bytes. Prices are kept as the integer on the
//! wire (four implied decimals, eight for the MWCB levels).

use std::io::{self, Read};

use crate::book::Side;

/// An eight-byte, space-padded stock symbol.
pub type Stock = [u8; 8];

/// `(type byte, name, specification section, length in bytes)` for every
/// message type in the specification.
pub const TYPES: [(u8, &str, &str, usize); 23] = [
    (b'S', "System Event", "1.1", 12),
    (b'R', "Stock Directory", "1.2.1", 39),
    (b'H', "Stock Trading Action", "1.2.2", 25),
    (b'Y', "Reg SHO Restriction", "1.2.3", 20),
    (b'L', "Market Participant Position", "1.2.4", 26),
    (b'V', "MWCB Decline Level", "1.2.5.1", 35),
    (b'W', "MWCB Status", "1.2.5.2", 12),
    (b'K', "IPO Quoting Period Update", "1.2.6", 28),
    (b'J', "LULD Auction Collar", "1.2.7", 35),
    (b'h', "Operational Halt", "1.2.8", 21),
    (b'A', "Add Order", "1.3.1", 36),
    (b'F', "Add Order with MPID", "1.3.2", 40),
    (b'E', "Order Executed", "1.4.1", 31),
    (b'C', "Order Executed With Price", "1.4.2", 36),
    (b'X', "Order Cancel", "1.4.3", 23),
    (b'D', "Order Delete", "1.4.4", 19),
    (b'U', "Order Replace", "1.4.5", 35),
    (b'P', "Trade (Non-Cross)", "1.5.1", 44),
    (b'Q', "Cross Trade", "1.5.2", 40),
    (b'B', "Broken Trade", "1.5.3", 19),
    (b'I', "NOII", "1.6", 50),
    (b'N', "RPII", "1.7", 20),
    (b'O', "DLCR Price Discovery", "1.8", 48),
];

/// Length in bytes of a message of this type, or `None` for an unknown type.
#[inline]
pub fn message_len(msg_type: u8) -> Option<usize> {
    Some(match msg_type {
        b'S' | b'W' => 12,
        b'R' => 39,
        b'H' => 25,
        b'Y' | b'N' => 20,
        b'L' => 26,
        b'V' | b'J' | b'U' => 35,
        b'K' => 28,
        b'h' => 21,
        b'A' | b'C' => 36,
        b'F' | b'Q' => 40,
        b'E' => 31,
        b'X' => 23,
        b'D' | b'B' => 19,
        b'P' => 44,
        b'I' => 50,
        b'O' => 48,
        _ => return None,
    })
}

/// The fields every message starts with (offsets 1 to 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Stock locate code; zero for messages not tied to one stock.
    pub locate: u16,
    /// Nasdaq internal tracking number.
    pub tracking: u16,
    /// Nanoseconds since midnight (48 bits on the wire).
    pub timestamp: u64,
}

/// The type-specific part of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    /// `S`, section 1.1.
    SystemEvent {
        /// `O`, `S`, `Q`, `M`, `E` or `C`.
        event: u8,
    },
    /// `R`, section 1.2.1.
    StockDirectory {
        /// Symbol.
        stock: Stock,
        /// Listing market.
        market_category: u8,
        /// Financial status indicator.
        financial_status: u8,
        /// Shares in a round lot.
        round_lot_size: u32,
        /// `Y` if only round lots are accepted.
        round_lots_only: u8,
        /// Issue classification (Appendix D).
        issue_classification: u8,
        /// Issue sub-type (Appendix E).
        issue_sub_type: [u8; 2],
        /// `P` for production, `T` for test.
        authenticity: u8,
        /// Short sale threshold indicator.
        short_sale_threshold: u8,
        /// IPO flag.
        ipo_flag: u8,
        /// LULD reference price tier.
        luld_tier: u8,
        /// ETP flag.
        etp_flag: u8,
        /// ETP leverage factor.
        etp_leverage: u32,
        /// Inverse ETP indicator.
        inverse: u8,
    },
    /// `H`, section 1.2.2.
    TradingAction {
        /// Symbol.
        stock: Stock,
        /// `H` halted, `P` paused, `Q` quotation only, `T` trading.
        state: u8,
        /// Reserved.
        reserved: u8,
        /// Reason code (Appendix C).
        reason: [u8; 4],
    },
    /// `Y`, section 1.2.3.
    RegSho {
        /// Symbol.
        stock: Stock,
        /// `0`, `1` or `2`.
        action: u8,
    },
    /// `L`, section 1.2.4.
    MarketParticipantPosition {
        /// Market participant identifier.
        mpid: [u8; 4],
        /// Symbol.
        stock: Stock,
        /// Primary market maker flag.
        primary_market_maker: u8,
        /// Market maker mode.
        market_maker_mode: u8,
        /// Market participant state.
        state: u8,
    },
    /// `V`, section 1.2.5.1. Levels are Price(8).
    MwcbDeclineLevel {
        /// Level 1.
        level1: u64,
        /// Level 2.
        level2: u64,
        /// Level 3.
        level3: u64,
    },
    /// `W`, section 1.2.5.2.
    MwcbStatus {
        /// `1`, `2` or `3`.
        breached_level: u8,
    },
    /// `K`, section 1.2.6.
    IpoQuotingPeriod {
        /// Symbol.
        stock: Stock,
        /// Release time, seconds since midnight.
        release_time: u32,
        /// `A` anticipated, `C` cancelled or postponed.
        qualifier: u8,
        /// IPO price, Price(4).
        price: u32,
    },
    /// `J`, section 1.2.7.
    LuldAuctionCollar {
        /// Symbol.
        stock: Stock,
        /// Reference price.
        reference_price: u32,
        /// Upper collar.
        upper_price: u32,
        /// Lower collar.
        lower_price: u32,
        /// Number of extensions.
        extension: u32,
    },
    /// `h`, section 1.2.8.
    OperationalHalt {
        /// Symbol.
        stock: Stock,
        /// `Q` Nasdaq, `B` BX, `X` PSX.
        market_code: u8,
        /// `H` halted, `T` resumed.
        action: u8,
    },
    /// `A` (section 1.3.1) when `mpid` is `None`, `F` (section 1.3.2) otherwise.
    AddOrder {
        /// Day-unique order reference number.
        order_ref: u64,
        /// Buy or sell.
        side: Side,
        /// Displayed shares.
        shares: u32,
        /// Symbol.
        stock: Stock,
        /// Display price, Price(4).
        price: u32,
        /// Attribution, present only on `F`.
        mpid: Option<[u8; 4]>,
    },
    /// `E`, section 1.4.1.
    OrderExecuted {
        /// Order reference number.
        order_ref: u64,
        /// Shares executed.
        shares: u32,
        /// Match number.
        match_number: u64,
    },
    /// `C`, section 1.4.2.
    OrderExecutedWithPrice {
        /// Order reference number.
        order_ref: u64,
        /// Shares executed.
        shares: u32,
        /// Match number.
        match_number: u64,
        /// `Y` printable, `N` non-printable.
        printable: u8,
        /// Execution price, Price(4).
        price: u32,
    },
    /// `X`, section 1.4.3.
    OrderCancel {
        /// Order reference number.
        order_ref: u64,
        /// Shares removed from the display size.
        shares: u32,
    },
    /// `D`, section 1.4.4.
    OrderDelete {
        /// Order reference number.
        order_ref: u64,
    },
    /// `U`, section 1.4.5.
    OrderReplace {
        /// Reference number of the order being replaced.
        old_ref: u64,
        /// Reference number of the replacement.
        new_ref: u64,
        /// New displayed shares.
        shares: u32,
        /// New display price, Price(4).
        price: u32,
    },
    /// `P`, section 1.5.1.
    Trade {
        /// Order reference number (zero since 2010).
        order_ref: u64,
        /// Buy/sell indicator, raw (always `B` since 2014).
        side: u8,
        /// Shares matched.
        shares: u32,
        /// Symbol.
        stock: Stock,
        /// Match price, Price(4).
        price: u32,
        /// Match number.
        match_number: u64,
    },
    /// `Q`, section 1.5.2.
    CrossTrade {
        /// Shares matched in the cross.
        shares: u64,
        /// Symbol.
        stock: Stock,
        /// Cross price, Price(4).
        price: u32,
        /// Match number.
        match_number: u64,
        /// `O` opening, `C` closing, `H` halt or IPO.
        cross_type: u8,
    },
    /// `B`, section 1.5.3.
    BrokenTrade {
        /// Match number of the broken execution.
        match_number: u64,
    },
    /// `I`, section 1.6.
    Noii {
        /// Paired shares.
        paired_shares: u64,
        /// Imbalance shares.
        imbalance_shares: u64,
        /// Imbalance direction.
        direction: u8,
        /// Symbol.
        stock: Stock,
        /// Far price.
        far_price: u32,
        /// Near price.
        near_price: u32,
        /// Current reference price.
        reference_price: u32,
        /// Cross type.
        cross_type: u8,
        /// Price variation indicator.
        variation: u8,
    },
    /// `N`, section 1.7.
    Rpii {
        /// Symbol.
        stock: Stock,
        /// Interest flag.
        interest: u8,
    },
    /// `O`, section 1.8.
    DlcrPriceDiscovery {
        /// Symbol.
        stock: Stock,
        /// Open eligibility status.
        eligibility: u8,
        /// Minimum allowable price.
        min_price: u32,
        /// Maximum allowable price.
        max_price: u32,
        /// Near execution price.
        near_price: u32,
        /// Near execution time.
        near_time: u64,
        /// Lower price range collar.
        lower_collar: u32,
        /// Upper price range collar.
        upper_collar: u32,
    },
}

/// One ITCH message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    /// Common leading fields.
    pub header: Header,
    /// Type-specific fields.
    pub body: Body,
}

/// Why a byte string is not an ITCH 5.0 message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// No bytes at all.
    Empty,
    /// The type byte is not in the specification.
    UnknownType(u8),
    /// The length does not match the type.
    BadLength {
        /// Type byte.
        msg_type: u8,
        /// Length the specification gives.
        expected: usize,
        /// Length received.
        got: usize,
    },
    /// The buy/sell indicator of an add order is neither `B` nor `S`.
    BadSide(u8),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Empty => write!(f, "empty message"),
            ParseError::UnknownType(t) => write!(f, "unknown message type 0x{t:02x}"),
            ParseError::BadLength {
                msg_type,
                expected,
                got,
            } => write!(
                f,
                "message type {:?} must be {expected} bytes, got {got}",
                *msg_type as char
            ),
            ParseError::BadSide(s) => write!(f, "buy/sell indicator 0x{s:02x}"),
        }
    }
}

impl std::error::Error for ParseError {}

#[inline(always)]
fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}

#[inline(always)]
fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

#[inline(always)]
fn be48(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes([0, 0, b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5]])
}

#[inline(always)]
fn be64(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes([
        b[o],
        b[o + 1],
        b[o + 2],
        b[o + 3],
        b[o + 4],
        b[o + 5],
        b[o + 6],
        b[o + 7],
    ])
}

#[inline(always)]
fn alpha<const N: usize>(b: &[u8], o: usize) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(&b[o..o + N]);
    out
}

#[inline(always)]
fn side(b: u8) -> Result<Side, ParseError> {
    match b {
        b'B' => Ok(Side::Buy),
        b'S' => Ok(Side::Sell),
        other => Err(ParseError::BadSide(other)),
    }
}

/// Parse one message. `b` is the message alone, without any length prefix.
#[inline]
pub fn parse(b: &[u8]) -> Result<Message, ParseError> {
    let Some(&msg_type) = b.first() else {
        return Err(ParseError::Empty);
    };
    let expected = message_len(msg_type).ok_or(ParseError::UnknownType(msg_type))?;
    if b.len() != expected {
        return Err(ParseError::BadLength {
            msg_type,
            expected,
            got: b.len(),
        });
    }
    let header = Header {
        locate: be16(b, 1),
        tracking: be16(b, 3),
        timestamp: be48(b, 5),
    };
    let body = match msg_type {
        b'A' => Body::AddOrder {
            order_ref: be64(b, 11),
            side: side(b[19])?,
            shares: be32(b, 20),
            stock: alpha(b, 24),
            price: be32(b, 32),
            mpid: None,
        },
        b'F' => Body::AddOrder {
            order_ref: be64(b, 11),
            side: side(b[19])?,
            shares: be32(b, 20),
            stock: alpha(b, 24),
            price: be32(b, 32),
            mpid: Some(alpha(b, 36)),
        },
        b'E' => Body::OrderExecuted {
            order_ref: be64(b, 11),
            shares: be32(b, 19),
            match_number: be64(b, 23),
        },
        b'C' => Body::OrderExecutedWithPrice {
            order_ref: be64(b, 11),
            shares: be32(b, 19),
            match_number: be64(b, 23),
            printable: b[31],
            price: be32(b, 32),
        },
        b'X' => Body::OrderCancel {
            order_ref: be64(b, 11),
            shares: be32(b, 19),
        },
        b'D' => Body::OrderDelete {
            order_ref: be64(b, 11),
        },
        b'U' => Body::OrderReplace {
            old_ref: be64(b, 11),
            new_ref: be64(b, 19),
            shares: be32(b, 27),
            price: be32(b, 31),
        },
        b'P' => Body::Trade {
            order_ref: be64(b, 11),
            side: b[19],
            shares: be32(b, 20),
            stock: alpha(b, 24),
            price: be32(b, 32),
            match_number: be64(b, 36),
        },
        b'Q' => Body::CrossTrade {
            shares: be64(b, 11),
            stock: alpha(b, 19),
            price: be32(b, 27),
            match_number: be64(b, 31),
            cross_type: b[39],
        },
        b'B' => Body::BrokenTrade {
            match_number: be64(b, 11),
        },
        b'S' => Body::SystemEvent { event: b[11] },
        b'R' => Body::StockDirectory {
            stock: alpha(b, 11),
            market_category: b[19],
            financial_status: b[20],
            round_lot_size: be32(b, 21),
            round_lots_only: b[25],
            issue_classification: b[26],
            issue_sub_type: alpha(b, 27),
            authenticity: b[29],
            short_sale_threshold: b[30],
            ipo_flag: b[31],
            luld_tier: b[32],
            etp_flag: b[33],
            etp_leverage: be32(b, 34),
            inverse: b[38],
        },
        b'H' => Body::TradingAction {
            stock: alpha(b, 11),
            state: b[19],
            reserved: b[20],
            reason: alpha(b, 21),
        },
        b'Y' => Body::RegSho {
            stock: alpha(b, 11),
            action: b[19],
        },
        b'L' => Body::MarketParticipantPosition {
            mpid: alpha(b, 11),
            stock: alpha(b, 15),
            primary_market_maker: b[23],
            market_maker_mode: b[24],
            state: b[25],
        },
        b'V' => Body::MwcbDeclineLevel {
            level1: be64(b, 11),
            level2: be64(b, 19),
            level3: be64(b, 27),
        },
        b'W' => Body::MwcbStatus {
            breached_level: b[11],
        },
        b'K' => Body::IpoQuotingPeriod {
            stock: alpha(b, 11),
            release_time: be32(b, 19),
            qualifier: b[23],
            price: be32(b, 24),
        },
        b'J' => Body::LuldAuctionCollar {
            stock: alpha(b, 11),
            reference_price: be32(b, 19),
            upper_price: be32(b, 23),
            lower_price: be32(b, 27),
            extension: be32(b, 31),
        },
        b'h' => Body::OperationalHalt {
            stock: alpha(b, 11),
            market_code: b[19],
            action: b[20],
        },
        b'I' => Body::Noii {
            paired_shares: be64(b, 11),
            imbalance_shares: be64(b, 19),
            direction: b[27],
            stock: alpha(b, 28),
            far_price: be32(b, 36),
            near_price: be32(b, 40),
            reference_price: be32(b, 44),
            cross_type: b[48],
            variation: b[49],
        },
        b'N' => Body::Rpii {
            stock: alpha(b, 11),
            interest: b[19],
        },
        b'O' => Body::DlcrPriceDiscovery {
            stock: alpha(b, 11),
            eligibility: b[19],
            min_price: be32(b, 20),
            max_price: be32(b, 24),
            near_price: be32(b, 28),
            near_time: be64(b, 32),
            lower_collar: be32(b, 40),
            upper_collar: be32(b, 44),
        },
        _ => unreachable!("message_len accepted the type"),
    };
    Ok(Message { header, body })
}

impl Message {
    /// The type byte this message has on the wire.
    pub fn type_byte(&self) -> u8 {
        match self.body {
            Body::SystemEvent { .. } => b'S',
            Body::StockDirectory { .. } => b'R',
            Body::TradingAction { .. } => b'H',
            Body::RegSho { .. } => b'Y',
            Body::MarketParticipantPosition { .. } => b'L',
            Body::MwcbDeclineLevel { .. } => b'V',
            Body::MwcbStatus { .. } => b'W',
            Body::IpoQuotingPeriod { .. } => b'K',
            Body::LuldAuctionCollar { .. } => b'J',
            Body::OperationalHalt { .. } => b'h',
            Body::AddOrder { mpid: None, .. } => b'A',
            Body::AddOrder { mpid: Some(_), .. } => b'F',
            Body::OrderExecuted { .. } => b'E',
            Body::OrderExecutedWithPrice { .. } => b'C',
            Body::OrderCancel { .. } => b'X',
            Body::OrderDelete { .. } => b'D',
            Body::OrderReplace { .. } => b'U',
            Body::Trade { .. } => b'P',
            Body::CrossTrade { .. } => b'Q',
            Body::BrokenTrade { .. } => b'B',
            Body::Noii { .. } => b'I',
            Body::Rpii { .. } => b'N',
            Body::DlcrPriceDiscovery { .. } => b'O',
        }
    }

    /// Append the wire form of this message to `out`, without a length prefix.
    /// The timestamp is truncated to 48 bits, as on the wire.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.type_byte());
        out.extend_from_slice(&self.header.locate.to_be_bytes());
        out.extend_from_slice(&self.header.tracking.to_be_bytes());
        out.extend_from_slice(&self.header.timestamp.to_be_bytes()[2..]);
        match self.body {
            Body::SystemEvent { event } => out.push(event),
            Body::StockDirectory {
                stock,
                market_category,
                financial_status,
                round_lot_size,
                round_lots_only,
                issue_classification,
                issue_sub_type,
                authenticity,
                short_sale_threshold,
                ipo_flag,
                luld_tier,
                etp_flag,
                etp_leverage,
                inverse,
            } => {
                out.extend_from_slice(&stock);
                out.push(market_category);
                out.push(financial_status);
                out.extend_from_slice(&round_lot_size.to_be_bytes());
                out.push(round_lots_only);
                out.push(issue_classification);
                out.extend_from_slice(&issue_sub_type);
                out.push(authenticity);
                out.push(short_sale_threshold);
                out.push(ipo_flag);
                out.push(luld_tier);
                out.push(etp_flag);
                out.extend_from_slice(&etp_leverage.to_be_bytes());
                out.push(inverse);
            }
            Body::TradingAction {
                stock,
                state,
                reserved,
                reason,
            } => {
                out.extend_from_slice(&stock);
                out.push(state);
                out.push(reserved);
                out.extend_from_slice(&reason);
            }
            Body::RegSho { stock, action } => {
                out.extend_from_slice(&stock);
                out.push(action);
            }
            Body::MarketParticipantPosition {
                mpid,
                stock,
                primary_market_maker,
                market_maker_mode,
                state,
            } => {
                out.extend_from_slice(&mpid);
                out.extend_from_slice(&stock);
                out.push(primary_market_maker);
                out.push(market_maker_mode);
                out.push(state);
            }
            Body::MwcbDeclineLevel {
                level1,
                level2,
                level3,
            } => {
                out.extend_from_slice(&level1.to_be_bytes());
                out.extend_from_slice(&level2.to_be_bytes());
                out.extend_from_slice(&level3.to_be_bytes());
            }
            Body::MwcbStatus { breached_level } => out.push(breached_level),
            Body::IpoQuotingPeriod {
                stock,
                release_time,
                qualifier,
                price,
            } => {
                out.extend_from_slice(&stock);
                out.extend_from_slice(&release_time.to_be_bytes());
                out.push(qualifier);
                out.extend_from_slice(&price.to_be_bytes());
            }
            Body::LuldAuctionCollar {
                stock,
                reference_price,
                upper_price,
                lower_price,
                extension,
            } => {
                out.extend_from_slice(&stock);
                out.extend_from_slice(&reference_price.to_be_bytes());
                out.extend_from_slice(&upper_price.to_be_bytes());
                out.extend_from_slice(&lower_price.to_be_bytes());
                out.extend_from_slice(&extension.to_be_bytes());
            }
            Body::OperationalHalt {
                stock,
                market_code,
                action,
            } => {
                out.extend_from_slice(&stock);
                out.push(market_code);
                out.push(action);
            }
            Body::AddOrder {
                order_ref,
                side,
                shares,
                stock,
                price,
                mpid,
            } => {
                out.extend_from_slice(&order_ref.to_be_bytes());
                out.push(match side {
                    Side::Buy => b'B',
                    Side::Sell => b'S',
                });
                out.extend_from_slice(&shares.to_be_bytes());
                out.extend_from_slice(&stock);
                out.extend_from_slice(&price.to_be_bytes());
                if let Some(mpid) = mpid {
                    out.extend_from_slice(&mpid);
                }
            }
            Body::OrderExecuted {
                order_ref,
                shares,
                match_number,
            } => {
                out.extend_from_slice(&order_ref.to_be_bytes());
                out.extend_from_slice(&shares.to_be_bytes());
                out.extend_from_slice(&match_number.to_be_bytes());
            }
            Body::OrderExecutedWithPrice {
                order_ref,
                shares,
                match_number,
                printable,
                price,
            } => {
                out.extend_from_slice(&order_ref.to_be_bytes());
                out.extend_from_slice(&shares.to_be_bytes());
                out.extend_from_slice(&match_number.to_be_bytes());
                out.push(printable);
                out.extend_from_slice(&price.to_be_bytes());
            }
            Body::OrderCancel { order_ref, shares } => {
                out.extend_from_slice(&order_ref.to_be_bytes());
                out.extend_from_slice(&shares.to_be_bytes());
            }
            Body::OrderDelete { order_ref } => {
                out.extend_from_slice(&order_ref.to_be_bytes());
            }
            Body::OrderReplace {
                old_ref,
                new_ref,
                shares,
                price,
            } => {
                out.extend_from_slice(&old_ref.to_be_bytes());
                out.extend_from_slice(&new_ref.to_be_bytes());
                out.extend_from_slice(&shares.to_be_bytes());
                out.extend_from_slice(&price.to_be_bytes());
            }
            Body::Trade {
                order_ref,
                side,
                shares,
                stock,
                price,
                match_number,
            } => {
                out.extend_from_slice(&order_ref.to_be_bytes());
                out.push(side);
                out.extend_from_slice(&shares.to_be_bytes());
                out.extend_from_slice(&stock);
                out.extend_from_slice(&price.to_be_bytes());
                out.extend_from_slice(&match_number.to_be_bytes());
            }
            Body::CrossTrade {
                shares,
                stock,
                price,
                match_number,
                cross_type,
            } => {
                out.extend_from_slice(&shares.to_be_bytes());
                out.extend_from_slice(&stock);
                out.extend_from_slice(&price.to_be_bytes());
                out.extend_from_slice(&match_number.to_be_bytes());
                out.push(cross_type);
            }
            Body::BrokenTrade { match_number } => {
                out.extend_from_slice(&match_number.to_be_bytes());
            }
            Body::Noii {
                paired_shares,
                imbalance_shares,
                direction,
                stock,
                far_price,
                near_price,
                reference_price,
                cross_type,
                variation,
            } => {
                out.extend_from_slice(&paired_shares.to_be_bytes());
                out.extend_from_slice(&imbalance_shares.to_be_bytes());
                out.push(direction);
                out.extend_from_slice(&stock);
                out.extend_from_slice(&far_price.to_be_bytes());
                out.extend_from_slice(&near_price.to_be_bytes());
                out.extend_from_slice(&reference_price.to_be_bytes());
                out.push(cross_type);
                out.push(variation);
            }
            Body::Rpii { stock, interest } => {
                out.extend_from_slice(&stock);
                out.push(interest);
            }
            Body::DlcrPriceDiscovery {
                stock,
                eligibility,
                min_price,
                max_price,
                near_price,
                near_time,
                lower_collar,
                upper_collar,
            } => {
                out.extend_from_slice(&stock);
                out.push(eligibility);
                out.extend_from_slice(&min_price.to_be_bytes());
                out.extend_from_slice(&max_price.to_be_bytes());
                out.extend_from_slice(&near_price.to_be_bytes());
                out.extend_from_slice(&near_time.to_be_bytes());
                out.extend_from_slice(&lower_collar.to_be_bytes());
                out.extend_from_slice(&upper_collar.to_be_bytes());
            }
        }
    }

    /// Append this message to `out` with the two-byte big-endian length prefix
    /// used by Nasdaq's sample files.
    pub fn encode_framed(&self, out: &mut Vec<u8>) {
        let at = out.len();
        out.extend_from_slice(&[0, 0]);
        self.encode(out);
        let len = (out.len() - at - 2) as u16;
        out[at..at + 2].copy_from_slice(&len.to_be_bytes());
    }
}

/// Pad a symbol with spaces to eight bytes.
pub fn stock(symbol: &str) -> Stock {
    let mut out = [b' '; 8];
    let bytes = symbol.as_bytes();
    let n = bytes.len().min(8);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

/// A symbol as text, without the padding.
pub fn stock_str(stock: &Stock) -> String {
    String::from_utf8_lossy(stock).trim_end().to_string()
}

/// Why reading a framed file stopped early.
#[derive(Debug)]
pub enum ReadError {
    /// The underlying reader failed.
    Io(io::Error),
    /// The file ends inside a length prefix or a message.
    Truncated {
        /// Byte offset of the incomplete frame.
        offset: u64,
    },
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "read failed: {e}"),
            ReadError::Truncated { offset } => {
                write!(f, "file ends inside the frame at byte {offset}")
            }
        }
    }
}

impl std::error::Error for ReadError {}

impl From<io::Error> for ReadError {
    fn from(e: io::Error) -> Self {
        ReadError::Io(e)
    }
}

/// Reads length-prefixed messages from any byte stream.
///
/// Nasdaq's sample files hold the day's messages back to back, each preceded
/// by its length as a two-byte big-endian integer. The framing is not part of
/// the ITCH specification; see `PAPER.md`.
pub struct FrameReader<R> {
    inner: R,
    buf: Vec<u8>,
    start: usize,
    end: usize,
    offset: u64,
    eof: bool,
}

impl<R: Read> FrameReader<R> {
    /// Wrap a reader with a one-mebibyte buffer.
    pub fn new(inner: R) -> Self {
        Self::with_capacity(inner, 1 << 20)
    }

    /// Wrap a reader with a buffer of `capacity` bytes (at least 128 KiB, so
    /// that any frame fits).
    pub fn with_capacity(inner: R, capacity: usize) -> Self {
        FrameReader {
            inner,
            buf: vec![0; capacity.max(1 << 17)],
            start: 0,
            end: 0,
            offset: 0,
            eof: false,
        }
    }

    /// Bytes consumed so far, including length prefixes.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    fn fill(&mut self, need: usize) -> Result<bool, ReadError> {
        while self.end - self.start < need {
            if self.eof {
                return Ok(false);
            }
            if self.start > 0 {
                self.buf.copy_within(self.start..self.end, 0);
                self.end -= self.start;
                self.start = 0;
            }
            match self.inner.read(&mut self.buf[self.end..]) {
                Ok(0) => self.eof = true,
                Ok(n) => self.end += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ReadError::Io(e)),
            }
        }
        Ok(true)
    }

    /// The next message, without its length prefix, or `None` at a clean end
    /// of file.
    #[inline]
    pub fn next_frame(&mut self) -> Result<Option<&[u8]>, ReadError> {
        if self.end - self.start < 2 && !self.fill(2)? {
            return if self.end == self.start {
                Ok(None)
            } else {
                Err(ReadError::Truncated {
                    offset: self.offset,
                })
            };
        }
        let len = usize::from(be16(&self.buf, self.start));
        if self.end - self.start < 2 + len && !self.fill(2 + len)? {
            return Err(ReadError::Truncated {
                offset: self.offset,
            });
        }
        let at = self.start + 2;
        self.start = at + len;
        self.offset += 2 + len as u64;
        Ok(Some(&self.buf[at..at + len]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_table_and_length_function_agree() {
        for (t, _, _, len) in TYPES {
            assert_eq!(message_len(t), Some(len), "type {}", t as char);
        }
        let known: Vec<u8> = TYPES.iter().map(|t| t.0).collect();
        for t in 0..=255u8 {
            if !known.contains(&t) {
                assert_eq!(message_len(t), None);
            }
        }
    }

    #[test]
    fn stock_pads_and_trims() {
        assert_eq!(&stock("AAPL"), b"AAPL    ");
        assert_eq!(stock_str(&stock("AAPL")), "AAPL");
        assert_eq!(&stock("TOOLONGNAME"), b"TOOLONGN");
    }

    #[test]
    fn frame_reader_handles_tiny_reads_and_truncation() {
        struct OneByte<'a>(&'a [u8]);
        impl Read for OneByte<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                match self.0.split_first() {
                    Some((b, rest)) => {
                        buf[0] = *b;
                        self.0 = rest;
                        Ok(1)
                    }
                    None => Ok(0),
                }
            }
        }
        let bytes = [0, 3, b'a', b'b', b'c', 0, 1, b'z'];
        let mut r = FrameReader::new(OneByte(&bytes));
        assert_eq!(r.next_frame().unwrap(), Some(&b"abc"[..]));
        assert_eq!(r.next_frame().unwrap(), Some(&b"z"[..]));
        assert_eq!(r.next_frame().unwrap(), None);
        assert_eq!(r.offset(), 8);

        let mut r = FrameReader::new(&bytes[..7]);
        r.next_frame().unwrap();
        assert!(matches!(
            r.next_frame(),
            Err(ReadError::Truncated { offset: 5 })
        ));
        let mut r = FrameReader::new(&bytes[..1]);
        assert!(matches!(
            r.next_frame(),
            Err(ReadError::Truncated { offset: 0 })
        ));
    }
}
