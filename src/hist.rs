//! A cycle counter and a log-linear histogram for per-message latency.
//!
//! On x86-64 the clock is the CPU time-stamp counter, read with `rdtsc` and
//! converted to nanoseconds by calibrating against the operating system clock
//! over the whole run. Elsewhere it falls back to `std::time::Instant`.

use std::time::Instant;

/// Values below this are counted exactly; above it, in 32 buckets per power
/// of two (about 3% wide).
const LINEAR: u64 = 64;
const SUB_BITS: u32 = 5;
const BUCKETS: usize = LINEAR as usize + (64 - 6) * (1 << SUB_BITS);

/// A histogram of unsigned values with bounded relative error.
#[derive(Clone)]
pub struct Histogram {
    counts: Vec<u64>,
    total: u64,
    max: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn bucket_of(v: u64) -> usize {
    if v < LINEAR {
        return v as usize;
    }
    let e = 63 - v.leading_zeros();
    let sub = (v >> (e - SUB_BITS)) & ((1 << SUB_BITS) - 1);
    LINEAR as usize + ((e - 6) as usize) * (1 << SUB_BITS) + sub as usize
}

/// Lowest value that lands in bucket `i`.
fn bucket_low(i: usize) -> u64 {
    if i < LINEAR as usize {
        return i as u64;
    }
    let j = i - LINEAR as usize;
    let e = (j >> SUB_BITS) as u32 + 6;
    let sub = (j & ((1 << SUB_BITS) - 1)) as u64;
    (1u64 << e) + (sub << (e - SUB_BITS))
}

impl Histogram {
    /// An empty histogram.
    pub fn new() -> Self {
        Histogram {
            counts: vec![0; BUCKETS],
            total: 0,
            max: 0,
        }
    }

    /// Count one value.
    #[inline]
    pub fn record(&mut self, v: u64) {
        self.counts[bucket_of(v)] += 1;
        self.total += 1;
        if v > self.max {
            self.max = v;
        }
    }

    /// Number of values recorded.
    pub fn count(&self) -> u64 {
        self.total
    }

    /// Largest value recorded, exact.
    pub fn max(&self) -> u64 {
        self.max
    }

    /// The value at quantile `q` in `0.0..=1.0`: the lower edge of the bucket
    /// holding the `ceil(q * count)`-th smallest value. Zero when empty.
    pub fn quantile(&self, q: f64) -> u64 {
        if self.total == 0 {
            return 0;
        }
        let rank = ((q * self.total as f64).ceil() as u64).clamp(1, self.total);
        let mut seen = 0;
        for (i, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                return bucket_low(i).min(self.max);
            }
        }
        self.max
    }

    /// Add another histogram's counts to this one.
    pub fn merge(&mut self, other: &Histogram) {
        for (a, b) in self.counts.iter_mut().zip(&other.counts) {
            *a += b;
        }
        self.total += other.total;
        self.max = self.max.max(other.max);
    }
}

/// Raw clock reading, in ticks.
#[inline(always)]
pub fn ticks() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: `rdtsc` has no preconditions on x86-64.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        use std::sync::OnceLock;
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_nanos() as u64
    }
}

/// Converts tick counts to nanoseconds.
#[derive(Debug, Clone, Copy)]
pub struct Calibration {
    start_ticks: u64,
    start: Instant,
}

impl Calibration {
    /// Note the two clocks now.
    pub fn start() -> Self {
        Calibration {
            start_ticks: ticks(),
            start: Instant::now(),
        }
    }

    /// Nanoseconds per tick since `start`.
    pub fn ns_per_tick(&self) -> f64 {
        let dt = ticks().wrapping_sub(self.start_ticks);
        let ns = self.start.elapsed().as_nanos() as f64;
        if dt == 0 {
            1.0
        } else {
            ns / dt as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_contiguous_and_ordered() {
        let mut last = 0;
        for i in 1..BUCKETS {
            let low = bucket_low(i);
            assert!(low > last, "bucket {i}");
            assert_eq!(bucket_of(low), i);
            assert_eq!(bucket_of(low - 1), i - 1);
            last = low;
        }
        assert_eq!(bucket_of(u64::MAX), BUCKETS - 1);
    }

    #[test]
    fn small_values_are_exact() {
        let mut h = Histogram::new();
        for v in 1..=50 {
            h.record(v);
        }
        assert_eq!(h.count(), 50);
        assert_eq!(h.quantile(0.5), 25);
        assert_eq!(h.quantile(1.0), 50);
        assert_eq!(h.quantile(0.0), 1);
        assert_eq!(h.max(), 50);
    }

    #[test]
    fn large_values_are_within_one_bucket() {
        let mut h = Histogram::new();
        for v in 1..=100_000u64 {
            h.record(v);
        }
        for q in [0.5, 0.9, 0.99, 0.999] {
            let exact = q * 100_000.0;
            let got = h.quantile(q) as f64;
            assert!(got <= exact && got >= exact * (1.0 - 1.0 / 32.0), "{q}");
        }
    }

    #[test]
    fn empty_and_merge() {
        let mut a = Histogram::new();
        assert_eq!(a.quantile(0.5), 0);
        let mut b = Histogram::new();
        a.record(10);
        b.record(1000);
        a.merge(&b);
        assert_eq!(a.count(), 2);
        assert_eq!(a.max(), 1000);
        assert_eq!(a.quantile(0.5), 10);
    }

    #[test]
    fn clock_moves_forward_and_calibrates() {
        let cal = Calibration::start();
        let a = ticks();
        let mut x = 0u64;
        for i in 0..2_000_000u64 {
            x = x.wrapping_add(std::hint::black_box(i));
        }
        std::hint::black_box(x);
        let b = ticks();
        assert!(b > a);
        let k = cal.ns_per_tick();
        assert!(k > 0.01 && k < 100.0, "ns per tick {k}");
    }
}
