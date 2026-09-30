//! DDSketch: a quantile sketch with relative-error guarantees.
//!
//! Reference: C. Masson, J. E. Rim, H. K. Lee, "DDSketch: A Fast and
//! Fully-Mergeable Quantile Sketch with Relative-Error Guarantees", VLDB 2019.
//!
//! Values are mapped to buckets `i = ⌈log_γ(x)⌉` with `γ = (1 + α)/(1 − α)`.
//! Returning `2γ^i/(γ + 1)` for a bucket guarantees a relative error of at
//! most `α` for every quantile whose bucket has not been collapsed. Memory is
//! bounded by collapsing the *lowest* buckets when a sketch would exceed its
//! bucket budget, which only degrades the accuracy of the lowest quantiles;
//! for latency, the high quantiles are the ones that matter.
//!
//! This implementation accepts non-negative values only (durations, sizes):
//! negative inputs are clamped to zero, which is also the right treatment for
//! the small negative durations produced by clock skew between hosts.

use serde::{Deserialize, Serialize};

/// Errors from sketch operations.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SketchError {
    /// The relative accuracy must lie in `(0, 1)`.
    #[error("relative accuracy must be in (0, 1), got {0}")]
    InvalidAccuracy(f64),
    /// Two sketches with different parameters cannot be merged.
    #[error("cannot merge sketches with different relative accuracy ({0} vs {1})")]
    IncompatibleAccuracy(f64, f64),
}

/// Smallest value that is mapped to a regular bucket; smaller values are
/// counted as zeros. One nanosecond when values are durations in nanoseconds.
const MIN_INDEXABLE: f64 = 1.0;

/// A DDSketch over non-negative values.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(from = "Repr", into = "Repr")]
pub struct DDSketch {
    alpha: f64,
    gamma: f64,
    inv_ln_gamma: f64,
    max_bins: usize,
    /// Bucket index of `bins[0]`.
    offset: i32,
    bins: Vec<u64>,
    zero_count: u64,
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
    collapsed: bool,
}

/// Compact serialised form: derived constants are recomputed on load and
/// leading/trailing empty bins are never stored.
#[derive(Serialize, Deserialize)]
struct Repr {
    alpha: f64,
    max_bins: u32,
    offset: i32,
    bins: Vec<u64>,
    zero_count: u64,
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
    collapsed: bool,
}

impl From<DDSketch> for Repr {
    fn from(s: DDSketch) -> Self {
        let first = s.bins.iter().position(|&c| c > 0);
        let (offset, bins) = match first {
            None => (0, Vec::new()),
            Some(first) => {
                let last = s.bins.iter().rposition(|&c| c > 0).unwrap_or(first);
                (s.offset + i32::try_from(first).unwrap_or(0), s.bins[first..=last].to_vec())
            }
        };
        Self {
            alpha: s.alpha,
            max_bins: u32::try_from(s.max_bins).unwrap_or(u32::MAX),
            offset,
            bins,
            zero_count: s.zero_count,
            count: s.count,
            sum: s.sum,
            min: s.min,
            max: s.max,
            collapsed: s.collapsed,
        }
    }
}

impl From<Repr> for DDSketch {
    fn from(r: Repr) -> Self {
        let alpha = if r.alpha > 0.0 && r.alpha < 1.0 { r.alpha } else { 0.01 };
        let gamma = (1.0 + alpha) / (1.0 - alpha);
        let mut s = Self {
            alpha,
            gamma,
            inv_ln_gamma: 1.0 / gamma.ln(),
            max_bins: (r.max_bins as usize).max(1),
            offset: r.offset,
            bins: r.bins,
            zero_count: r.zero_count,
            count: r.count,
            sum: r.sum,
            min: r.min,
            max: r.max,
            collapsed: r.collapsed,
        };
        // Never trust a deserialised sketch to respect its own budget.
        s.enforce_budget();
        s
    }
}

impl Default for DDSketch {
    fn default() -> Self {
        Self::new(0.01, 2048).unwrap_or_else(|_| unreachable!("default parameters are valid"))
    }
}

impl DDSketch {
    /// Creates an empty sketch with relative accuracy `alpha` that keeps at
    /// most `max_bins` buckets.
    ///
    /// # Errors
    /// Returns [`SketchError::InvalidAccuracy`] if `alpha` is not in `(0, 1)`.
    pub fn new(alpha: f64, max_bins: usize) -> Result<Self, SketchError> {
        if !(alpha > 0.0 && alpha < 1.0) {
            return Err(SketchError::InvalidAccuracy(alpha));
        }
        let gamma = (1.0 + alpha) / (1.0 - alpha);
        Ok(Self {
            alpha,
            gamma,
            inv_ln_gamma: 1.0 / gamma.ln(),
            max_bins: max_bins.max(1),
            offset: 0,
            bins: Vec::new(),
            zero_count: 0,
            count: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            collapsed: false,
        })
    }

    /// The relative accuracy guarantee.
    #[must_use]
    pub const fn relative_accuracy(&self) -> f64 {
        self.alpha
    }

    #[allow(clippy::cast_possible_truncation)]
    fn index_of(&self, x: f64) -> i32 {
        (x.ln() * self.inv_ln_gamma).ceil() as i32
    }

    fn value_of(&self, index: i32) -> f64 {
        2.0 * self.gamma.powi(index) / (self.gamma + 1.0)
    }

    /// Adds one observation.
    pub fn add(&mut self, x: f64) {
        self.add_n(x, 1);
    }

    /// Adds `n` identical observations.
    pub fn add_n(&mut self, x: f64, n: u64) {
        if n == 0 || !x.is_finite() {
            return;
        }
        let x = if x.is_sign_negative() { 0.0 } else { x };
        self.count += n;
        #[allow(clippy::cast_precision_loss)]
        {
            self.sum += x * n as f64;
        }
        self.min = self.min.min(x);
        self.max = self.max.max(x);
        if x < MIN_INDEXABLE {
            self.zero_count += n;
            return;
        }
        let idx = self.index_of(x);
        let slot = self.slot_for(idx);
        self.bins[slot] += n;
    }

    /// Returns the position of bucket `idx` in `bins`, growing (and, if
    /// necessary, collapsing) the store.
    fn slot_for(&mut self, idx: i32) -> usize {
        if self.bins.is_empty() {
            self.offset = idx;
            self.bins.push(0);
            return 0;
        }
        if idx < self.offset {
            if self.collapsed_floor().is_some_and(|floor| idx <= floor) {
                return 0;
            }
            let grow = usize::try_from(i64::from(self.offset) - i64::from(idx)).unwrap_or(0);
            let mut new_bins = vec![0; grow];
            new_bins.extend_from_slice(&self.bins);
            self.bins = new_bins;
            self.offset = idx;
        }
        let pos = usize::try_from(i64::from(idx) - i64::from(self.offset)).unwrap_or(0);
        if pos >= self.bins.len() {
            self.bins.resize(pos + 1, 0);
        }
        if self.bins.len() > self.max_bins {
            self.enforce_budget();
            // The target may have been folded into the lowest bucket.
            let pos = i64::from(idx) - i64::from(self.offset);
            return usize::try_from(pos.max(0)).unwrap_or(0);
        }
        pos
    }

    /// If the lowest bucket has absorbed collapsed buckets, its index.
    fn collapsed_floor(&self) -> Option<i32> {
        self.collapsed.then_some(self.offset)
    }

    /// Collapses the lowest buckets until at most `max_bins` remain.
    fn enforce_budget(&mut self) {
        if self.bins.len() <= self.max_bins {
            return;
        }
        let excess = self.bins.len() - self.max_bins;
        let folded: u64 = self.bins[..=excess].iter().sum();
        self.bins.drain(..excess);
        self.bins[0] = folded;
        self.offset += i32::try_from(excess).unwrap_or(i32::MAX);
        self.collapsed = true;
    }

    /// Merges another sketch into this one.
    ///
    /// # Errors
    /// Returns [`SketchError::IncompatibleAccuracy`] if the sketches were
    /// created with different relative accuracy.
    pub fn merge(&mut self, other: &Self) -> Result<(), SketchError> {
        if (self.alpha - other.alpha).abs() > 1e-15 {
            return Err(SketchError::IncompatibleAccuracy(self.alpha, other.alpha));
        }
        if other.count == 0 {
            return Ok(());
        }
        self.count += other.count;
        self.zero_count += other.zero_count;
        self.sum += other.sum;
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
        self.collapsed |= other.collapsed;
        for (i, &c) in other.bins.iter().enumerate() {
            if c == 0 {
                continue;
            }
            let idx = other.offset + i32::try_from(i).unwrap_or(i32::MAX);
            let slot = self.slot_for(idx);
            self.bins[slot] += c;
        }
        Ok(())
    }

    /// Number of observations.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// Whether the sketch is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Exact sum of observations.
    #[must_use]
    pub const fn sum(&self) -> f64 {
        self.sum
    }

    /// Exact mean (NaN when empty).
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn mean(&self) -> f64 {
        if self.count == 0 { f64::NAN } else { self.sum / self.count as f64 }
    }

    /// Exact minimum (NaN when empty).
    #[must_use]
    pub fn min(&self) -> f64 {
        if self.count == 0 { f64::NAN } else { self.min }
    }

    /// Exact maximum (NaN when empty).
    #[must_use]
    pub fn max(&self) -> f64 {
        if self.count == 0 { f64::NAN } else { self.max }
    }

    /// Number of stored buckets (a proxy for memory use).
    #[must_use]
    pub fn bucket_count(&self) -> usize {
        self.bins.len()
    }

    /// Whether low buckets were collapsed to respect the bucket budget.
    #[must_use]
    pub const fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    /// Estimates the `q`-quantile, `q` in `[0, 1]` (NaN when empty).
    ///
    /// The estimate is within a factor `1 ± α` of the order statistic of rank
    /// `⌊q (n − 1)⌋`, unless that rank falls into collapsed buckets.
    #[must_use]
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn quantile(&self, q: f64) -> f64 {
        if self.count == 0 || q.is_nan() {
            return f64::NAN;
        }
        let q = q.clamp(0.0, 1.0);
        if q == 0.0 {
            return self.min;
        }
        if q >= 1.0 {
            return self.max;
        }
        let rank = (q * (self.count - 1) as f64).floor() as u64;
        if rank < self.zero_count {
            return 0.0_f64.max(self.min);
        }
        let mut seen = self.zero_count;
        for (i, &c) in self.bins.iter().enumerate() {
            seen += c;
            if seen > rank {
                let idx = self.offset + i32::try_from(i).unwrap_or(i32::MAX);
                return self.value_of(idx).clamp(self.min, self.max);
            }
        }
        self.max
    }

    /// Resets the sketch to empty, keeping its parameters and allocation.
    pub fn clear(&mut self) {
        self.bins.clear();
        self.offset = 0;
        self.zero_count = 0;
        self.count = 0;
        self.sum = 0.0;
        self.min = f64::INFINITY;
        self.max = f64::NEG_INFINITY;
        self.collapsed = false;
    }
}

impl PartialEq for DDSketch {
    fn eq(&self, other: &Self) -> bool {
        let a = Repr::from(self.clone());
        let b = Repr::from(other.clone());
        a.alpha.to_bits() == b.alpha.to_bits()
            && a.max_bins == b.max_bins
            && a.offset == b.offset
            && a.bins == b.bins
            && a.zero_count == b.zero_count
            && a.count == b.count
            && a.min.to_bits() == b.min.to_bits()
            && a.max.to_bits() == b.max.to_bits()
            && a.collapsed == b.collapsed
            // Sums are compared with a tolerance: floating-point addition is
            // not associative, and merge order must not matter.
            && (a.sum - b.sum).abs() <= 1e-9 * a.sum.abs().max(b.sum.abs()).max(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;
    use proptest::prelude::*;

    fn exact_quantile(sorted: &[f64], q: f64) -> f64 {
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let rank = (q * (sorted.len() - 1) as f64).floor() as usize;
        sorted[rank]
    }

    #[test]
    fn rejects_invalid_accuracy() {
        assert!(DDSketch::new(0.0, 10).is_err());
        assert!(DDSketch::new(1.0, 10).is_err());
        assert!(DDSketch::new(f64::NAN, 10).is_err());
    }

    #[test]
    fn empty_sketch() {
        let s = DDSketch::default();
        assert!(s.is_empty());
        assert!(s.quantile(0.5).is_nan());
        assert!(s.mean().is_nan());
    }

    #[test]
    fn relative_error_on_lognormal_latencies() {
        let mut rng = Rng::seed_from_u64(41);
        let mut s = DDSketch::new(0.01, 2048).unwrap();
        let mut xs: Vec<f64> = (0..50_000).map(|_| rng.lognormal(15.0, 1.2)).collect();
        for &x in &xs {
            s.add(x);
        }
        xs.sort_by(f64::total_cmp);
        for q in [0.01, 0.1, 0.5, 0.9, 0.95, 0.99, 0.999] {
            let want = exact_quantile(&xs, q);
            let got = s.quantile(q);
            assert!((got - want).abs() <= 0.01 * want + 1e-9, "q={q}: {got} vs {want}");
        }
        assert_eq!(s.count(), 50_000);
        assert!((s.min() - xs[0]).abs() < 1e-9);
        assert!((s.max() - xs[xs.len() - 1]).abs() < 1e-9);
    }

    #[test]
    fn zeros_and_negatives_are_counted_as_zero() {
        let mut s = DDSketch::default();
        s.add(0.0);
        s.add(-5.0);
        s.add(0.5);
        s.add(100.0);
        assert_eq!(s.count(), 4);
        assert!(s.quantile(0.25).abs() < 1e-12);
        assert!((s.quantile(1.0) - 100.0).abs() < 1e-9);
        s.add(f64::NAN);
        s.add(f64::INFINITY);
        assert_eq!(s.count(), 4);
    }

    #[test]
    fn collapsing_bounds_memory_and_keeps_high_quantiles() {
        // 64 buckets at 5% accuracy cover a dynamic range of about 600x:
        // quantiles within that range of the maximum stay accurate, the
        // lowest ones are folded into the floor bucket.
        let mut rng = Rng::seed_from_u64(42);
        let mut s = DDSketch::new(0.05, 64).unwrap();
        let mut xs: Vec<f64> = (0..20_000).map(|_| rng.lognormal(10.0, 1.0)).collect();
        for &x in &xs {
            s.add(x);
        }
        assert!(s.bucket_count() <= 64);
        assert!(s.is_collapsed());
        xs.sort_by(f64::total_cmp);
        for q in [0.5, 0.9, 0.99, 0.999] {
            let want = exact_quantile(&xs, q);
            let got = s.quantile(q);
            assert!((got - want).abs() <= 0.05 * want, "q={q}: {got} vs {want}");
        }
        let low = exact_quantile(&xs, 0.0001);
        assert!(s.quantile(0.0001) > 1.05 * low, "the lowest quantiles are collapsed");
    }

    #[test]
    fn serde_round_trip_is_compact() {
        let mut s = DDSketch::default();
        for x in [10.0, 20.0, 30.0, 1e6] {
            s.add(x);
        }
        let bytes = postcard::to_stdvec(&s).unwrap();
        let back: DDSketch = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(s, back);
        assert!((back.quantile(0.5) - s.quantile(0.5)).abs() < 1e-12);
        assert!(bytes.len() < 1500, "{} bytes", bytes.len());
    }

    #[test]
    fn merge_rejects_different_accuracy() {
        let mut a = DDSketch::new(0.01, 100).unwrap();
        let b = DDSketch::new(0.02, 100).unwrap();
        assert!(a.merge(&b).is_err());
    }

    fn sketch_of(xs: &[f64], max_bins: usize) -> DDSketch {
        let mut s = DDSketch::new(0.02, max_bins).unwrap();
        xs.iter().for_each(|&x| s.add(x));
        s
    }

    proptest! {
        #[test]
        fn merge_equals_single_sketch(
            a in prop::collection::vec(0.0f64..1e7, 0..300),
            b in prop::collection::vec(0.0f64..1e7, 0..300),
        ) {
            let mut merged = sketch_of(&a, 4096);
            merged.merge(&sketch_of(&b, 4096)).unwrap();
            let all: Vec<f64> = a.iter().chain(&b).copied().collect();
            let single = sketch_of(&all, 4096);
            prop_assert_eq!(&merged, &single);
        }

        #[test]
        fn merge_is_commutative_and_associative_even_when_collapsing(
            a in prop::collection::vec(0.0f64..1e9, 1..200),
            b in prop::collection::vec(0.0f64..1e9, 1..200),
            c in prop::collection::vec(0.0f64..1e9, 1..200),
        ) {
            let (sa, sb, sc) = (sketch_of(&a, 32), sketch_of(&b, 32), sketch_of(&c, 32));
            let mut ab_c = sa.clone();
            ab_c.merge(&sb).unwrap();
            ab_c.merge(&sc).unwrap();
            let mut bc = sb.clone();
            bc.merge(&sc).unwrap();
            let mut a_bc = sa.clone();
            a_bc.merge(&bc).unwrap();
            let mut c_ba = sc.clone();
            c_ba.merge(&sb).unwrap();
            c_ba.merge(&sa).unwrap();
            prop_assert!(ab_c.bucket_count() <= 32);
            prop_assert_eq!(ab_c.count(), a_bc.count());
            // High quantiles agree exactly regardless of merge order.
            prop_assert_eq!(ab_c.quantile(0.99).to_bits(), a_bc.quantile(0.99).to_bits());
            prop_assert_eq!(ab_c.quantile(0.99).to_bits(), c_ba.quantile(0.99).to_bits());
        }

        #[test]
        fn quantiles_are_within_relative_error(xs in prop::collection::vec(1.0f64..1e9, 1..500), q in 0.0f64..=1.0) {
            let s = sketch_of(&xs, 4096);
            let mut sorted = xs;
            sorted.sort_by(f64::total_cmp);
            let want = if q >= 1.0 { sorted[sorted.len() - 1] } else { exact_quantile(&sorted, q) };
            let got = s.quantile(q);
            prop_assert!((got - want).abs() <= 0.02 * want + 1e-9, "q={} got={} want={}", q, got, want);
        }
    }
}
