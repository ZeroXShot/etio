//! Order statistics on slices.
//!
//! These functions are used on batch data (for example the reference window
//! of a root-cause analysis). They ignore NaN values, never allocate more than
//! one scratch copy, and run in expected linear time using selection instead
//! of sorting.

/// Copies the finite values of `xs` into a new vector.
fn finite(xs: &[f64]) -> Vec<f64> {
    xs.iter().copied().filter(|x| x.is_finite()).collect()
}

/// Selects the `k`-th smallest element (0-based) of `v` in expected `O(n)`.
fn select(v: &mut [f64], k: usize) -> f64 {
    let (_, x, _) = v.select_nth_unstable_by(k, f64::total_cmp);
    *x
}

/// Quantile with linear interpolation between order statistics
/// (Hyndman & Fan type 7, the default of NumPy and R).
///
/// Returns NaN if there are no finite values.
#[must_use]
pub fn quantile(xs: &[f64], q: f64) -> f64 {
    let mut v = finite(xs);
    quantile_in_place(&mut v, q)
}

/// Like [`quantile`], but reorders `v` instead of copying it. `v` must not contain NaN.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn quantile_in_place(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let q = q.clamp(0.0, 1.0);
    let pos = q * (v.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let frac = pos - lo as f64;
    let a = select(v, lo);
    if frac == 0.0 || lo + 1 >= v.len() {
        return a;
    }
    // After selection, every element right of `lo` is >= a; the next order
    // statistic is the minimum of that suffix.
    let b = v[lo + 1..].iter().copied().fold(f64::INFINITY, f64::min);
    a + frac * (b - a)
}

/// Median of the finite values (NaN if none).
#[must_use]
pub fn median(xs: &[f64]) -> f64 {
    quantile(xs, 0.5)
}

/// Median absolute deviation around the median (unscaled).
///
/// Multiply by [`super::MAD_TO_SIGMA`] to estimate a standard deviation.
#[must_use]
pub fn mad(xs: &[f64]) -> f64 {
    let mut v = finite(xs);
    if v.is_empty() {
        return f64::NAN;
    }
    let m = quantile_in_place(&mut v, 0.5);
    for x in &mut v {
        *x = (*x - m).abs();
    }
    quantile_in_place(&mut v, 0.5)
}

/// Interquartile range `Q3 − Q1` of the finite values (NaN if none).
#[must_use]
pub fn iqr(xs: &[f64]) -> f64 {
    let mut v = finite(xs);
    if v.is_empty() {
        return f64::NAN;
    }
    let q1 = quantile_in_place(&mut v, 0.25);
    let q3 = quantile_in_place(&mut v, 0.75);
    q3 - q1
}

/// Location and scale summary of a reference sample.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RobustSummary {
    /// Number of finite values.
    pub n: usize,
    /// Median.
    pub median: f64,
    /// Median absolute deviation (unscaled).
    pub mad: f64,
    /// Interquartile range.
    pub iqr: f64,
    /// Arithmetic mean.
    pub mean: f64,
    /// Sample standard deviation.
    pub std_dev: f64,
    /// Minimum.
    pub min: f64,
    /// Maximum.
    pub max: f64,
}

impl RobustSummary {
    /// Summarises the finite values of `xs`. Returns `None` if there are none.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn of(xs: &[f64]) -> Option<Self> {
        let mut v = finite(xs);
        if v.is_empty() {
            return None;
        }
        let n = v.len();
        let mean = v.iter().sum::<f64>() / n as f64;
        let var = if n > 1 { v.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1) as f64 } else { 0.0 };
        let min = v.iter().copied().fold(f64::INFINITY, f64::min);
        let max = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let q1 = quantile_in_place(&mut v, 0.25);
        let q3 = quantile_in_place(&mut v, 0.75);
        let median = quantile_in_place(&mut v, 0.5);
        for x in &mut v {
            *x = (*x - median).abs();
        }
        let mad = quantile_in_place(&mut v, 0.5);
        Some(Self { n, median, mad, iqr: q3 - q1, mean, std_dev: var.sqrt(), min, max })
    }

    /// A robust estimate of the standard deviation: the larger of the scaled
    /// MAD and the scaled IQR, which keeps working when more than half of the
    /// sample is identical (MAD = 0) but the quartiles still differ.
    #[must_use]
    pub fn robust_sigma(&self) -> f64 {
        (self.mad * super::MAD_TO_SIGMA).max(self.iqr * super::IQR_TO_SIGMA)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn naive_quantile(xs: &[f64], q: f64) -> f64 {
        let mut v: Vec<f64> = xs.iter().copied().filter(|x| x.is_finite()).collect();
        v.sort_by(f64::total_cmp);
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            let pos = q * (v.len() - 1) as f64;
            let lo = pos.floor() as usize;
            let hi = pos.ceil() as usize;
            v[lo] + (pos - lo as f64) * (v[hi] - v[lo])
        }
    }

    #[test]
    fn basic_values() {
        let xs = [1.0, 2.0, 3.0, 4.0, 100.0];
        assert!((median(&xs) - 3.0).abs() < 1e-12);
        assert!((mad(&xs) - 1.0).abs() < 1e-12);
        assert!((iqr(&xs) - 2.0).abs() < 1e-12);
        assert!((quantile(&[1.0, 2.0], 0.5) - 1.5).abs() < 1e-12);
        assert!(median(&[]).is_nan());
        assert!(median(&[f64::NAN]).is_nan());
        assert!((median(&[f64::NAN, 5.0]) - 5.0).abs() < 1e-12);
    }

    #[test]
    fn robust_sigma_handles_mostly_constant_samples() {
        // 60% zeros: MAD is 0 but IQR is not.
        let xs = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 4.0];
        let s = RobustSummary::of(&xs).unwrap();
        assert!(s.mad.abs() < 1e-12);
        assert!(s.robust_sigma() > 0.0);
    }

    proptest! {
        #[test]
        fn quantile_matches_sorting(xs in prop::collection::vec(-1e6f64..1e6, 1..200), q in 0.0f64..=1.0) {
            let got = quantile(&xs, q);
            let want = naive_quantile(&xs, q);
            prop_assert!((got - want).abs() <= 1e-9 * want.abs().max(1.0), "{got} vs {want}");
        }

        #[test]
        fn summary_is_consistent(xs in prop::collection::vec(-1e3f64..1e3, 1..100)) {
            let s = RobustSummary::of(&xs).unwrap();
            prop_assert!(s.min <= s.median && s.median <= s.max);
            prop_assert!(s.mad >= 0.0 && s.iqr >= 0.0);
            prop_assert!((s.median - naive_quantile(&xs, 0.5)).abs() < 1e-9);
        }
    }
}
