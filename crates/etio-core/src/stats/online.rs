//! Online moments.

use serde::{Deserialize, Serialize};

/// Welford's numerically stable running mean and variance.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Welford {
    n: u64,
    mean: f64,
    m2: f64,
}

impl Welford {
    /// An empty accumulator.
    #[must_use]
    pub const fn new() -> Self {
        Self { n: 0, mean: 0.0, m2: 0.0 }
    }

    /// Adds an observation (non-finite values are ignored).
    #[allow(clippy::cast_precision_loss)]
    pub fn push(&mut self, x: f64) {
        if !x.is_finite() {
            return;
        }
        self.n += 1;
        let delta = x - self.mean;
        self.mean += delta / self.n as f64;
        self.m2 += delta * (x - self.mean);
    }

    /// Merges another accumulator (Chan et al. parallel formula).
    #[allow(clippy::cast_precision_loss)]
    pub fn merge(&mut self, other: &Self) {
        if other.n == 0 {
            return;
        }
        if self.n == 0 {
            *self = *other;
            return;
        }
        let n = self.n + other.n;
        let delta = other.mean - self.mean;
        let (na, nb, nn) = (self.n as f64, other.n as f64, n as f64);
        self.mean += delta * nb / nn;
        self.m2 += other.m2 + delta * delta * na * nb / nn;
        self.n = n;
    }

    /// Number of observations.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.n
    }

    /// Mean (NaN when empty).
    #[must_use]
    pub fn mean(&self) -> f64 {
        if self.n == 0 { f64::NAN } else { self.mean }
    }

    /// Sample variance (NaN with fewer than two observations).
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn variance(&self) -> f64 {
        if self.n < 2 { f64::NAN } else { self.m2 / (self.n - 1) as f64 }
    }

    /// Sample standard deviation.
    #[must_use]
    pub fn std_dev(&self) -> f64 {
        self.variance().sqrt()
    }
}

/// Exponentially weighted mean and variance with a configurable half-life.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ewm {
    alpha: f64,
    mean: f64,
    var: f64,
    initialised: bool,
}

impl Ewm {
    /// Creates an estimator whose weights halve every `half_life` observations.
    #[must_use]
    pub fn with_half_life(half_life: f64) -> Self {
        let alpha = 1.0 - 0.5f64.powf(1.0 / half_life.max(1e-9));
        Self { alpha, mean: 0.0, var: 0.0, initialised: false }
    }

    /// Adds an observation (non-finite values are ignored).
    pub fn push(&mut self, x: f64) {
        if !x.is_finite() {
            return;
        }
        if !self.initialised {
            self.mean = x;
            self.var = 0.0;
            self.initialised = true;
            return;
        }
        let delta = x - self.mean;
        self.mean += self.alpha * delta;
        self.var = (1.0 - self.alpha) * (self.var + self.alpha * delta * delta);
    }

    /// Current mean (NaN before the first observation).
    #[must_use]
    pub fn mean(&self) -> f64 {
        if self.initialised { self.mean } else { f64::NAN }
    }

    /// Current variance (NaN before the first observation).
    #[must_use]
    pub fn variance(&self) -> f64 {
        if self.initialised { self.var } else { f64::NAN }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    #[test]
    fn welford_matches_two_pass() {
        let mut rng = Rng::seed_from_u64(31);
        let xs: Vec<f64> = (0..1000).map(|_| rng.normal(1e6, 3.0)).collect();
        let mut w = Welford::new();
        xs.iter().for_each(|&x| w.push(x));
        let mean = xs.iter().sum::<f64>() / 1000.0;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / 999.0;
        assert!((w.mean() - mean).abs() < 1e-6);
        assert!((w.variance() - var).abs() / var < 1e-9);
    }

    #[test]
    fn welford_merge_equals_sequential() {
        let mut rng = Rng::seed_from_u64(32);
        let xs: Vec<f64> = (0..500).map(|_| rng.normal(5.0, 2.0)).collect();
        let (mut a, mut b, mut all) = (Welford::new(), Welford::new(), Welford::new());
        xs[..123].iter().for_each(|&x| a.push(x));
        xs[123..].iter().for_each(|&x| b.push(x));
        xs.iter().for_each(|&x| all.push(x));
        a.merge(&b);
        assert_eq!(a.count(), all.count());
        assert!((a.mean() - all.mean()).abs() < 1e-12);
        assert!((a.variance() - all.variance()).abs() < 1e-9);
        let mut empty = Welford::new();
        empty.merge(&all);
        assert_eq!(empty, all);
    }

    #[test]
    fn ewm_tracks_level_changes() {
        let mut e = Ewm::with_half_life(10.0);
        assert!(e.mean().is_nan());
        for _ in 0..100 {
            e.push(1.0);
        }
        assert!((e.mean() - 1.0).abs() < 1e-12);
        for _ in 0..10 {
            e.push(3.0);
        }
        // After one half-life the gap has halved.
        assert!((e.mean() - 2.0).abs() < 1e-9, "{}", e.mean());
    }
}
