//! Tables and the chart written by the reproduce command.
//!
//! Everything here is plain text built by hand: CSV with `\n` line ends and
//! one SVG. Given the same inputs the output is byte-identical.

use std::fmt::Write as _;

use crate::itch::{stock_str, TYPES};
use crate::replay::{Audit, Replay, Stats};

/// Nanoseconds since midnight as `HH:MM:SS.nnnnnnnnn`.
pub fn clock(ns: u64) -> String {
    let s = ns / 1_000_000_000;
    format!(
        "{:02}:{:02}:{:02}.{:09}",
        s / 3600,
        (s / 60) % 60,
        s % 60,
        ns % 1_000_000_000
    )
}

/// A Price(4) integer as dollars with four decimals.
pub fn price(p: u32) -> String {
    format!("{}.{:04}", p / 10_000, p % 10_000)
}

/// Message counts by type, in specification order.
pub fn messages_csv(stats: &Stats) -> String {
    let mut out = String::from("type,name,spec_section,bytes_each,count,share\n");
    let total = stats.messages.max(1) as f64;
    for (t, name, section, len) in TYPES {
        let count = stats.counts[usize::from(t)];
        let _ = writeln!(
            out,
            "{},{},{},{},{},{:.6}",
            t as char,
            name,
            section,
            len,
            count,
            count as f64 / total
        );
    }
    let _ = writeln!(out, "all,All messages,,,{},1.000000", stats.messages);
    out
}

/// The invariant report as `check,value` rows.
pub fn invariants_csv(stats: &Stats, audit: &Audit, parse_errors: u64) -> String {
    let mut out = String::from("check,value\n");
    let mut row = |k: &str, v: String| {
        let _ = writeln!(out, "{k},{v}");
    };
    row("messages", stats.messages.to_string());
    row("first_timestamp", clock(stats.first_ts));
    row("last_timestamp", clock(stats.last_ts));
    row("parse_errors", parse_errors.to_string());
    row("time_reversals", stats.time_reversals.to_string());
    row("unknown_order_ref", stats.unknown_ref.to_string());
    row("duplicate_order_ref", stats.duplicate_ref.to_string());
    row("over_reduce", stats.over_reduce.to_string());
    row("locate_mismatch", stats.locate_mismatch.to_string());
    row("zero_share_add", stats.zero_share_add.to_string());
    row("orders_added", stats.orders_added.to_string());
    row("peak_live_orders", stats.peak_live_orders.to_string());
    row("shares_added", stats.shares_added.to_string());
    row("shares_executed", stats.shares_executed.to_string());
    row("shares_cancelled", stats.shares_cancelled.to_string());
    row("shares_deleted", stats.shares_deleted.to_string());
    row("shares_replaced", stats.shares_replaced.to_string());
    row("shares_resting_at_end", audit.resting_shares.to_string());
    row("ledger_gap", audit.ledger_gap.to_string());
    row("orders_resting_at_end", audit.resting_orders.to_string());
    row("order_refs_live_at_end", audit.refs_live.to_string());
    row(
        "books_with_orders_at_end",
        audit.books_with_orders.to_string(),
    );
    row("structure_errors", audit.structure_errors.len().to_string());
    row("volume_conserved", audit.conserved().to_string());
    row("book_updates", stats.book_updates.to_string());
    row("crossed_updates", stats.crossed_updates.to_string());
    row("locked_updates", stats.locked_updates.to_string());
    row("crossed_episodes", stats.crossed_episodes.to_string());
    row(
        "crossed_episodes_state_trading",
        stats.crossed_episodes_trading.to_string(),
    );
    row(
        "crossed_episodes_before_0930",
        stats.crossed_episodes_by_session[0].to_string(),
    );
    row(
        "crossed_episodes_0930_to_1600",
        stats.crossed_episodes_by_session[1].to_string(),
    );
    row(
        "crossed_episodes_after_1600",
        stats.crossed_episodes_by_session[2].to_string(),
    );
    row(
        "crossed_seconds_summed_over_books",
        format!("{:.3}", stats.crossed_ns as f64 / 1e9),
    );
    row("books_crossed_at_end", audit.crossed_at_end.to_string());
    row("executions_E", stats.exec_e.to_string());
    row(
        "executions_E_at_best_price",
        stats.exec_e_at_best.to_string(),
    );
    row(
        "executions_E_oldest_at_best_price",
        stats.exec_e_at_front.to_string(),
    );
    row(
        "executions_E_not_oldest_before_0930",
        stats.exec_e_not_front_by_session[0].to_string(),
    );
    row(
        "executions_E_not_oldest_0930_to_1600",
        stats.exec_e_not_front_by_session[1].to_string(),
    );
    row(
        "executions_E_not_oldest_after_1600",
        stats.exec_e_not_front_by_session[2].to_string(),
    );
    row(
        "executions_E_not_oldest_state_not_trading",
        stats.exec_e_not_front_not_trading.to_string(),
    );
    row("executions_C", stats.exec_c.to_string());
    row(
        "executions_C_at_best_price",
        stats.exec_c_at_best.to_string(),
    );
    row(
        "executions_C_oldest_at_best_price",
        stats.exec_c_at_front.to_string(),
    );
    row("printed_shares", stats.printed_shares.to_string());
    row(
        "non_printable_shares",
        stats.non_printable_shares.to_string(),
    );
    row("cross_shares", stats.cross_shares.to_string());
    row("book_digest", format!("{:016x}", audit.digest));
    out
}

/// Stocks that were ever crossed, longest crossed first, at most `limit` rows.
pub fn crossed_symbols_csv(replay: &Replay, limit: usize) -> String {
    let mut rows = replay.cross_totals();
    rows.sort_by(|a, b| b.1.ns.cmp(&a.1.ns).then(a.0.cmp(&b.0)));
    let mut out = String::from("symbol,locate,episodes,crossed_updates,crossed_seconds\n");
    for (locate, t) in rows.into_iter().take(limit) {
        let symbol = replay.symbol(locate).map(stock_str).unwrap_or_default();
        let _ = writeln!(
            out,
            "{},{},{},{},{:.6}",
            symbol,
            locate,
            t.episodes,
            t.updates,
            t.ns as f64 / 1e9
        );
    }
    out
}

/// The first crossed episodes, one row each.
pub fn crossed_examples_csv(replay: &Replay) -> String {
    let mut out = String::from("time,symbol,trading_state,message_type,best_bid,best_ask\n");
    for e in &replay.examples {
        let symbol = replay.symbol(e.locate).map(stock_str).unwrap_or_default();
        let state = if e.state == 0 { '-' } else { e.state as char };
        let _ = writeln!(
            out,
            "{},{},{},{},{},{}",
            clock(e.timestamp),
            symbol,
            state,
            e.msg_type as char,
            price(e.bid),
            price(e.ask)
        );
    }
    out
}

/// Latency percentiles for one group of messages, in nanoseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct LatencyRow {
    /// Row label, for example `Add order (A)`.
    pub label: String,
    /// Messages measured.
    pub count: u64,
    /// Median.
    pub p50: f64,
    /// 90th percentile.
    pub p90: f64,
    /// 99th percentile.
    pub p99: f64,
    /// 99.9th percentile.
    pub p999: f64,
    /// 99.99th percentile.
    pub p9999: f64,
    /// Largest single measurement.
    pub max: f64,
}

/// The latency table as CSV.
pub fn latency_csv(rows: &[LatencyRow]) -> String {
    let mut out = String::from("messages,count,p50_ns,p90_ns,p99_ns,p99.9_ns,p99.99_ns,max_ns\n");
    for r in rows {
        let _ = writeln!(
            out,
            "{},{},{:.0},{:.0},{:.0},{:.0},{:.0},{:.0}",
            r.label, r.count, r.p50, r.p90, r.p99, r.p999, r.p9999, r.max
        );
    }
    out
}

/// A duration in nanoseconds with a unit a person would pick.
pub fn human_ns(ns: f64) -> String {
    if ns < 999.5 {
        format!("{ns:.0} ns")
    } else if ns < 9_995.0 {
        format!("{:.2} \u{b5}s", ns / 1e3)
    } else if ns < 99_950.0 {
        format!("{:.1} \u{b5}s", ns / 1e3)
    } else if ns < 999_500.0 {
        format!("{:.0} \u{b5}s", ns / 1e3)
    } else {
        format!("{:.2} ms", ns / 1e6)
    }
}

fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A dot plot of p50, p99 and p99.9 per row on a log axis, as SVG.
///
/// Colours are the first three slots of a palette checked for colour-vision
/// deficiency; each percentile also has its own marker shape, and the exact
/// values are printed beside the plot.
pub fn latency_svg(rows: &[LatencyRow], title: &str, subtitle: &str) -> String {
    const SURFACE: &str = "#fcfcfb";
    const INK: &str = "#0b0b0b";
    const INK2: &str = "#52514e";
    const GRID: &str = "#e4e3df";
    const SERIES: [&str; 3] = ["#2a78d6", "#eb6834", "#1baf7a"];
    const NAMES: [&str; 3] = ["p50", "p99", "p99.9"];
    const FONT: &str = "system-ui, -apple-system, 'Segoe UI', Helvetica, Arial, sans-serif";

    let (label_w, plot_w, col_w) = (220.0, 300.0, 76.0);
    let plot_x = label_w + 16.0;
    let cols_x = plot_x + plot_w + 24.0;
    let width = cols_x + 3.0 * col_w + 8.0;
    let top = 96.0;
    let row_h = 34.0;
    let height = top + row_h * rows.len() as f64 + 44.0;

    let lo = rows
        .iter()
        .map(|r| r.p50.max(1.0))
        .fold(f64::INFINITY, f64::min);
    let hi = rows.iter().map(|r| r.p999.max(1.0)).fold(1.0, f64::max);
    let (lo, hi) = if lo.is_finite() {
        (lo, hi)
    } else {
        (1.0, 10.0)
    };
    let d0 = lo.log10().floor();
    let d1 = hi.log10().ceil().max(d0 + 1.0);
    let x = |ns: f64| plot_x + (ns.max(1.0).log10() - d0) / (d1 - d0) * plot_w;

    let mut s = String::new();
    let _ = writeln!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width:.0} {height:.0}\" \
         width=\"{width:.0}\" height=\"{height:.0}\" font-family=\"{FONT}\" role=\"img\" \
         aria-label=\"{}\">",
        esc(title)
    );
    let _ = writeln!(
        s,
        "<rect width=\"{width:.0}\" height=\"{height:.0}\" fill=\"{SURFACE}\"/>"
    );
    let _ = writeln!(
        s,
        "<text x=\"16\" y=\"28\" font-size=\"16\" font-weight=\"600\" fill=\"{INK}\">{}</text>",
        esc(title)
    );
    let _ = writeln!(
        s,
        "<text x=\"16\" y=\"48\" font-size=\"12\" fill=\"{INK2}\">{}</text>",
        esc(subtitle)
    );

    // Legend, above the plot.
    let mut lx = plot_x;
    for i in 0..3 {
        marker(&mut s, i, lx + 5.0, 70.0, SERIES[i], SURFACE);
        let _ = writeln!(
            s,
            "<text x=\"{:.1}\" y=\"74\" font-size=\"12\" fill=\"{INK2}\">{}</text>",
            lx + 15.0,
            NAMES[i]
        );
        lx += 70.0;
    }
    // Column heads for the printed values.
    for (i, name) in NAMES.iter().enumerate() {
        let _ = writeln!(
            s,
            "<text x=\"{:.1}\" y=\"74\" font-size=\"12\" fill=\"{INK2}\" text-anchor=\"end\">{}</text>",
            cols_x + col_w * (i as f64 + 1.0) - 8.0,
            name
        );
    }

    // Decade grid.
    let bottom = top + row_h * rows.len() as f64;
    let mut d = d0;
    while d <= d1 + 1e-9 {
        let gx = x(10f64.powf(d));
        let _ = writeln!(
            s,
            "<line x1=\"{gx:.1}\" y1=\"{:.1}\" x2=\"{gx:.1}\" y2=\"{bottom:.1}\" stroke=\"{GRID}\" stroke-width=\"1\"/>",
            top - 8.0
        );
        let _ = writeln!(
            s,
            "<text x=\"{gx:.1}\" y=\"{:.1}\" font-size=\"11\" fill=\"{INK2}\" text-anchor=\"middle\">{}</text>",
            bottom + 16.0,
            human_ns(10f64.powf(d))
        );
        d += 1.0;
    }
    let _ = writeln!(
        s,
        "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"11\" fill=\"{INK2}\" text-anchor=\"middle\">time to parse and apply one message (log scale)</text>",
        plot_x + plot_w / 2.0,
        bottom + 34.0
    );

    for (i, r) in rows.iter().enumerate() {
        let cy = top + row_h * (i as f64 + 0.5);
        let _ = writeln!(
            s,
            "<text x=\"16\" y=\"{:.1}\" font-size=\"13\" fill=\"{INK}\">{}</text>",
            cy - 1.0,
            esc(&r.label)
        );
        let _ = writeln!(
            s,
            "<text x=\"16\" y=\"{:.1}\" font-size=\"10.5\" fill=\"{INK2}\">{} messages</text>",
            cy + 12.0,
            group_thousands(r.count)
        );
        let values = [r.p50, r.p99, r.p999];
        let _ = writeln!(
            s,
            "<line x1=\"{:.1}\" y1=\"{cy:.1}\" x2=\"{:.1}\" y2=\"{cy:.1}\" stroke=\"{INK2}\" stroke-opacity=\"0.45\" stroke-width=\"2\"/>",
            x(values[0]),
            x(values[2])
        );
        for (k, v) in values.iter().enumerate() {
            marker(&mut s, k, x(*v), cy, SERIES[k], SURFACE);
            let _ = writeln!(
                s,
                "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"12\" fill=\"{INK}\" text-anchor=\"end\">{}</text>",
                cols_x + col_w * (k as f64 + 1.0) - 8.0,
                cy + 4.0,
                human_ns(*v)
            );
        }
    }
    s.push_str("</svg>\n");
    s
}

/// Circle, square or triangle with a surface-coloured ring.
fn marker(s: &mut String, shape: usize, cx: f64, cy: f64, fill: &str, ring: &str) {
    let _ = match shape {
        0 => writeln!(
            s,
            "<circle cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"5\" fill=\"{fill}\" stroke=\"{ring}\" stroke-width=\"2\"/>"
        ),
        1 => writeln!(
            s,
            "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"9\" height=\"9\" fill=\"{fill}\" stroke=\"{ring}\" stroke-width=\"2\"/>",
            cx - 4.5,
            cy - 4.5
        ),
        _ => writeln!(
            s,
            "<path d=\"M{:.1} {:.1} L{:.1} {:.1} L{:.1} {:.1} Z\" fill=\"{fill}\" stroke=\"{ring}\" stroke-width=\"2\"/>",
            cx,
            cy - 6.0,
            cx + 6.0,
            cy + 5.0,
            cx - 6.0,
            cy + 5.0
        ),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_and_price_format() {
        assert_eq!(clock(34_200_000_000_123), "09:30:00.000000123");
        assert_eq!(price(1_234_500), "123.4500");
        assert_eq!(price(1), "0.0001");
    }

    #[test]
    fn human_units() {
        assert_eq!(human_ns(42.0), "42 ns");
        assert_eq!(human_ns(1_500.0), "1.50 \u{b5}s");
        assert_eq!(human_ns(25_000.0), "25.0 \u{b5}s");
        assert_eq!(human_ns(3_000_000.0), "3.00 ms");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
        assert_eq!(group_thousands(999), "999");
    }

    #[test]
    fn svg_is_well_formed_enough_and_deterministic() {
        let rows = vec![
            LatencyRow {
                label: "Add order (A)".into(),
                count: 1_000,
                p50: 40.0,
                p90: 80.0,
                p99: 300.0,
                p999: 2_000.0,
                p9999: 9_000.0,
                max: 1e6,
            },
            LatencyRow {
                label: "Delete <D> & co".into(),
                count: 10,
                p50: 30.0,
                p90: 50.0,
                p99: 100.0,
                p999: 900.0,
                p9999: 950.0,
                max: 1e3,
            },
        ];
        let a = latency_svg(&rows, "Title", "Subtitle");
        assert_eq!(a, latency_svg(&rows, "Title", "Subtitle"));
        assert!(a.starts_with("<svg ") && a.ends_with("</svg>\n"));
        assert!(a.contains("Delete &lt;D&gt; &amp; co"));
        assert_eq!(a.matches("<circle").count(), 3);
        // An empty table still renders.
        assert!(latency_svg(&[], "t", "s").contains("</svg>"));
    }

    #[test]
    fn message_table_lists_every_type() {
        let mut stats = Stats {
            messages: 4,
            ..Stats::default()
        };
        stats.counts[usize::from(b'A')] = 3;
        stats.counts[usize::from(b'D')] = 1;
        let csv = messages_csv(&stats);
        assert_eq!(csv.lines().count(), 25);
        assert!(csv.contains("A,Add Order,1.3.1,36,3,0.750000\n"));
    }
}
