//! An exact sliding window of order statistics.
//!
//! Detectors need the median and the MAD of the last *n* observations of
//! every series, updated once per aggregation window. Approximate streaming
//! quantiles would work, but the windows are small (hundreds of points), so an
//! exact structure is both simpler to reason about and faster:
//!
//! * the values are kept sorted in a `Vec`; an update is two binary searches
//!   and two `memmove`s of at most `n` floats (a few hundred nanoseconds);
//! * any quantile is then `O(1)`;
//! * the MAD is `O(log n)`: the absolute deviations from the median are the
//!   merge of two sorted sequences (the values left of the median, read
//!   backwards, and the values right of it), so their median is a
//!   "k-th element of two sorted arrays" query, answered by binary search
//!   without materialising the deviations.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// A fixed-capacity sliding window with exact order statistics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SortedWindow {
    capacity: usize,
    order: VecDeque<f64>,
    sorted: Vec<f64>,
}

impl SortedWindow {
    /// Creates an empty window that keeps the last `capacity` values.
    ///
    /// # Panics
    /// Panics if `capacity` is zero.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "window capacity must be positive");
        Self { capacity, order: VecDeque::with_capacity(capacity), sorted: Vec::with_capacity(capacity) }
    }

    /// Adds a value, evicting and returning the oldest one if the window is full.
    ///
    /// Non-finite values are ignored (missing data must be handled by the
    /// caller, not smuggled into order statistics) and `None` is returned.
    pub fn push(&mut self, x: f64) -> Option<f64> {
        if !x.is_finite() {
            return None;
        }
        // Canonicalise -0.0 so that equal values have identical bits.
        let x = if x == 0.0 { 0.0 } else { x };
        let evicted = if self.order.len() == self.capacity {
            let old = self.order.pop_front()?;
            let idx = self.sorted.partition_point(|v| v.total_cmp(&old).is_lt());
            debug_assert!(idx < self.sorted.len() && self.sorted[idx].to_bits() == old.to_bits());
            self.sorted.remove(idx);
            Some(old)
        } else {
            None
        };
        let idx = self.sorted.partition_point(|v| v.total_cmp(&x).is_lt());
        self.sorted.insert(idx, x);
        self.order.push_back(x);
        evicted
    }

    /// Number of values currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sorted.len()
    }

    /// Whether the window holds no values.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sorted.is_empty()
    }

    /// Whether the window holds `capacity` values.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.sorted.len() == self.capacity
    }

    /// Maximum number of values held.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Removes all values.
    pub fn clear(&mut self) {
        self.order.clear();
        self.sorted.clear();
    }

    /// The values in insertion order, oldest first.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = f64> + '_ {
        self.order.iter().copied()
    }

    /// The values in ascending order.
    #[must_use]
    pub fn sorted(&self) -> &[f64] {
        &self.sorted
    }

    /// Quantile with linear interpolation (type 7). NaN if empty.
    #[must_use]
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn quantile(&self, q: f64) -> f64 {
        let n = self.sorted.len();
        if n == 0 {
            return f64::NAN;
        }
        let pos = q.clamp(0.0, 1.0) * (n - 1) as f64;
        let lo = pos.floor() as usize;
        let hi = (lo + 1).min(n - 1);
        let frac = pos - lo as f64;
        self.sorted[lo] + frac * (self.sorted[hi] - self.sorted[lo])
    }

    /// Median. NaN if empty.
    #[must_use]
    pub fn median(&self) -> f64 {
        self.quantile(0.5)
    }

    /// Sample standard deviation (NaN with fewer than two values). `O(n)`,
    /// computed exactly on demand to avoid the drift of running sums.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn std_dev(&self) -> f64 {
        let n = self.sorted.len();
        if n < 2 {
            return f64::NAN;
        }
        let mean = self.sorted.iter().sum::<f64>() / n as f64;
        let ss: f64 = self.sorted.iter().map(|x| (x - mean) * (x - mean)).sum();
        (ss / (n - 1) as f64).sqrt()
    }

    /// Interquartile range. NaN if empty.
    #[must_use]
    pub fn iqr(&self) -> f64 {
        self.quantile(0.75) - self.quantile(0.25)
    }

    /// Median absolute deviation around the median (unscaled), in `O(log n)`.
    /// NaN if empty.
    #[must_use]
    pub fn mad(&self) -> f64 {
        let n = self.sorted.len();
        if n == 0 {
            return f64::NAN;
        }
        let m = self.median();
        if n % 2 == 1 {
            kth_deviation(&self.sorted, m, n / 2)
        } else {
            0.5 * (kth_deviation(&self.sorted, m, n / 2 - 1) + kth_deviation(&self.sorted, m, n / 2))
        }
    }
}

/// The `k`-th smallest (0-based) value of `|x - m|` over a sorted slice.
fn kth_deviation(sorted: &[f64], m: f64, k: usize) -> f64 {
    let n = sorted.len();
    debug_assert!(k < n);
    // Left sequence: m - sorted[s-1-j] (ascending in j); right: sorted[s+j] - m.
    let s = sorted.partition_point(|v| *v < m);
    let (nl, nr) = (s, n - s);
    let left = |j: usize| m - sorted[s - 1 - j];
    let right = |j: usize| sorted[s + j] - m;

    // Take `i` elements from the left sequence and `need - i` from the right.
    let need = k + 1;
    let mut lo = need.saturating_sub(nr);
    let mut hi = need.min(nl);
    while lo < hi {
        let i = lo + (hi - lo) / 2;
        // Inside the loop i < nl and need - i >= 1, so both reads are in bounds.
        if left(i) < right(need - i - 1) {
            lo = i + 1;
        } else {
            hi = i;
        }
    }
    let i = lo;
    let from_left = if i > 0 { left(i - 1) } else { f64::NEG_INFINITY };
    let from_right = if need > i { right(need - i - 1) } else { f64::NEG_INFINITY };
    from_left.max(from_right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::robust;
    use proptest::prelude::*;

    #[test]
    fn evicts_in_insertion_order() {
        let mut w = SortedWindow::new(3);
        assert_eq!(w.push(5.0), None);
        assert_eq!(w.push(1.0), None);
        assert_eq!(w.push(3.0), None);
        assert!(w.is_full());
        assert_eq!(w.push(4.0), Some(5.0));
        assert_eq!(w.sorted(), &[1.0, 3.0, 4.0]);
        assert_eq!(w.iter().collect::<Vec<_>>(), vec![1.0, 3.0, 4.0]);
        assert_eq!(w.push(f64::NAN), None);
        assert_eq!(w.len(), 3);
    }

    #[test]
    fn statistics_of_small_window() {
        let mut w = SortedWindow::new(10);
        for x in [1.0, 2.0, 3.0, 4.0, 100.0] {
            w.push(x);
        }
        assert!((w.median() - 3.0).abs() < 1e-12);
        assert!((w.mad() - 1.0).abs() < 1e-12);
        assert!((w.iqr() - 2.0).abs() < 1e-12);
        assert!(SortedWindow::new(1).mad().is_nan());
    }

    #[test]
    fn standard_deviation() {
        let mut w = SortedWindow::new(4);
        for x in [2.0, 4.0, 4.0, 6.0] {
            w.push(x);
        }
        assert!((w.std_dev() - (8.0f64 / 3.0).sqrt()).abs() < 1e-12);
        assert!(SortedWindow::new(2).std_dev().is_nan());
    }

    #[test]
    fn handles_duplicates_and_signed_zero() {
        let mut w = SortedWindow::new(4);
        for x in [0.0, -0.0, 0.0, -0.0, 0.0, 2.0] {
            w.push(x);
        }
        assert_eq!(w.len(), 4);
        assert!(w.median().abs() < 1e-12);
        assert!(w.mad().abs() < 1e-12);
    }

    proptest! {
        #[test]
        fn matches_batch_statistics(
            xs in prop::collection::vec(prop_oneof![-50.0f64..50.0, Just(0.0), Just(1.0)], 1..400),
            cap in 1usize..64,
        ) {
            let mut w = SortedWindow::new(cap);
            for (i, &x) in xs.iter().enumerate() {
                w.push(x);
                let start = (i + 1).saturating_sub(cap);
                let tail = &xs[start..=i];
                let want_med = robust::median(tail);
                let want_mad = robust::mad(tail);
                let want_iqr = robust::iqr(tail);
                prop_assert!((w.median() - want_med).abs() < 1e-9);
                prop_assert!((w.mad() - want_mad).abs() < 1e-9, "mad {} vs {}", w.mad(), want_mad);
                prop_assert!((w.iqr() - want_iqr).abs() < 1e-9);
            }
        }
    }
}
