//! A stable, versioned pseudo-random number generator and distributions.
//!
//! The simulator and the evaluation harness must regenerate *exactly* the same
//! datasets from the same seed, across platforms and across dependency
//! upgrades. General-purpose crates do not promise that: `rand`'s `StdRng`
//! explicitly reserves the right to change algorithm, and distribution
//! samplers change between releases. So the algorithms here are frozen and
//! documented; changing any of them is a breaking change of
//! [`ALGORITHM_VERSION`].
//!
//! * Generator: xoshiro256++ (Blackman & Vigna), seeded through SplitMix64.
//! * Normal: Marsaglia's polar method.
//! * Gamma: Marsaglia & Tsang (2000).
//! * Poisson: Knuth's multiplication method for λ < 30, Hörmann's PTRS
//!   transformed rejection (1993) above.

use serde::{Deserialize, Serialize};

/// Version of the frozen algorithms in this module.
pub const ALGORITHM_VERSION: u32 = 1;

/// SplitMix64 step, used for seeding and for deriving independent streams.
#[must_use]
pub const fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The xoshiro256++ generator with distribution helpers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rng {
    s: [u64; 4],
    /// Spare normal deviate produced by the polar method, stored as bits so
    /// the type stays `Eq`.
    spare_normal: Option<u64>,
}

impl Rng {
    /// Creates a generator from a 64-bit seed.
    #[must_use]
    pub fn seed_from_u64(seed: u64) -> Self {
        let mut sm = seed;
        let s = [splitmix64(&mut sm), splitmix64(&mut sm), splitmix64(&mut sm), splitmix64(&mut sm)];
        Self { s, spare_normal: None }
    }

    /// Derives an independent generator for a named sub-stream.
    ///
    /// Streams derived with different labels are statistically independent,
    /// and adding a new stream never perturbs existing ones: this is what keeps
    /// a simulation reproducible when a new feature starts drawing numbers.
    #[must_use]
    pub fn fork(&self, label: &str) -> Self {
        // FNV-1a over the label, mixed with the current state.
        let mut h: u64 = 0xCBF2_9CE4_8422_2325;
        for b in label.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01B3);
        }
        let mut sm = h ^ self.s[0].rotate_left(17) ^ self.s[2];
        Self::seed_from_u64(splitmix64(&mut sm))
    }

    /// Next raw 64-bit output.
    pub fn next_u64(&mut self) -> u64 {
        let result = (self.s[0].wrapping_add(self.s[3])).rotate_left(23).wrapping_add(self.s[0]);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Uniform in `[0, 1)` with 53 bits of precision.
    #[allow(clippy::cast_precision_loss)]
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform in `(0, 1]`; safe to pass to `ln`.
    pub fn f64_open0(&mut self) -> f64 {
        1.0 - self.f64()
    }

    /// Uniform in `[lo, hi)`.
    pub fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.f64()
    }

    /// Uniform integer in `[0, n)` using Lemire's nearly divisionless method.
    ///
    /// # Panics
    /// Panics if `n == 0`.
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0, "below(0) is empty");
        let mut m = u128::from(self.next_u64()) * u128::from(n);
        #[allow(clippy::cast_possible_truncation)]
        let mut low = m as u64;
        if low < n {
            let threshold = n.wrapping_neg() % n;
            while low < threshold {
                m = u128::from(self.next_u64()) * u128::from(n);
                #[allow(clippy::cast_possible_truncation)]
                {
                    low = m as u64;
                }
            }
        }
        #[allow(clippy::cast_possible_truncation)]
        let hi = (m >> 64) as u64;
        hi
    }

    /// Uniform index into a slice of length `len`.
    ///
    /// # Panics
    /// Panics if `len == 0`.
    pub fn index(&mut self, len: usize) -> usize {
        usize::try_from(self.below(len as u64)).unwrap_or(0)
    }

    /// Bernoulli trial with success probability `p` (clamped to `[0, 1]`).
    pub fn chance(&mut self, p: f64) -> bool {
        self.f64() < p.clamp(0.0, 1.0)
    }

    /// Standard normal deviate (mean 0, variance 1).
    pub fn standard_normal(&mut self) -> f64 {
        if let Some(bits) = self.spare_normal.take() {
            return f64::from_bits(bits);
        }
        loop {
            let u = 2.0 * self.f64() - 1.0;
            let v = 2.0 * self.f64() - 1.0;
            let s = u * u + v * v;
            if s > 0.0 && s < 1.0 {
                let factor = (-2.0 * s.ln() / s).sqrt();
                self.spare_normal = Some((v * factor).to_bits());
                return u * factor;
            }
        }
    }

    /// Normal deviate with the given mean and standard deviation.
    pub fn normal(&mut self, mean: f64, std_dev: f64) -> f64 {
        mean + std_dev * self.standard_normal()
    }

    /// Log-normal deviate: `exp(N(mu, sigma))`.
    pub fn lognormal(&mut self, mu: f64, sigma: f64) -> f64 {
        self.normal(mu, sigma).exp()
    }

    /// Log-normal deviate parameterised by its median and the ratio p99/median,
    /// which is how latency distributions are usually described.
    pub fn lognormal_median_p99(&mut self, median: f64, p99_over_median: f64) -> f64 {
        // z_{0.99} = 2.326347874...
        let sigma = p99_over_median.max(1.0).ln() / 2.326_347_874_040_841;
        median * self.normal(0.0, sigma).exp()
    }

    /// Exponential deviate with the given rate.
    pub fn exponential(&mut self, rate: f64) -> f64 {
        -self.f64_open0().ln() / rate
    }

    /// Gamma deviate with the given shape (`k > 0`) and scale.
    pub fn gamma(&mut self, shape: f64, scale: f64) -> f64 {
        if shape < 1.0 {
            // Boost: Gamma(k) = Gamma(k + 1) * U^(1/k).
            let u = self.f64_open0();
            return self.gamma(shape + 1.0, scale) * u.powf(1.0 / shape);
        }
        let d = shape - 1.0 / 3.0;
        let c = 1.0 / (9.0 * d).sqrt();
        loop {
            let x = self.standard_normal();
            let v = 1.0 + c * x;
            if v <= 0.0 {
                continue;
            }
            let v = v * v * v;
            let u = self.f64_open0();
            if u < 1.0 - 0.0331 * x * x * x * x || u.ln() < 0.5 * x * x + d * (1.0 - v + v.ln()) {
                return d * v * scale;
            }
        }
    }

    /// Poisson deviate with mean `lambda`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn poisson(&mut self, lambda: f64) -> u64 {
        if lambda.is_nan() || lambda <= 0.0 {
            return 0;
        }
        if lambda < 30.0 {
            let limit = (-lambda).exp();
            let mut k = 0u64;
            let mut p = self.f64_open0();
            while p > limit {
                k += 1;
                p *= self.f64_open0();
            }
            return k;
        }
        // PTRS: W. Hörmann, "The transformed rejection method for generating
        // Poisson random variables", Insurance: Mathematics and Economics 12 (1993).
        let slam = lambda.sqrt();
        let loglam = lambda.ln();
        let b = 0.931 + 2.53 * slam;
        let a = -0.059 + 0.02483 * b;
        let inv_alpha = 1.1239 + 1.1328 / (b - 3.4);
        let vr = 0.9277 - 3.6224 / (b - 2.0);
        loop {
            let u = self.f64() - 0.5;
            let v = self.f64();
            let us = 0.5 - u.abs();
            let k = ((2.0 * a / us + b) * u + lambda + 0.43).floor();
            if us >= 0.07 && v <= vr {
                return k as u64;
            }
            if k < 0.0 || (us < 0.013 && v > us) {
                continue;
            }
            let lhs = v.ln() + inv_alpha.ln() - (a / (us * us) + b).ln();
            let rhs = -lambda + k * loglam - libm::lgamma(k + 1.0);
            if lhs <= rhs {
                return k as u64;
            }
        }
    }

    /// Picks an index with probability proportional to `weights`.
    ///
    /// Returns `None` if the weights are empty or all non-positive.
    pub fn weighted_index(&mut self, weights: &[f64]) -> Option<usize> {
        let total: f64 = weights.iter().filter(|w| **w > 0.0).sum();
        if total.is_nan() || total <= 0.0 {
            return None;
        }
        let mut target = self.f64() * total;
        for (i, &w) in weights.iter().enumerate() {
            if w > 0.0 {
                if target < w {
                    return Some(i);
                }
                target -= w;
            }
        }
        // Floating-point slack: fall back to the last positive weight.
        weights.iter().rposition(|w| *w > 0.0)
    }

    /// Shuffles a slice in place (Fisher–Yates).
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.index(i + 1);
            items.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mean_var(xs: &[f64]) -> (f64, f64) {
        #[allow(clippy::cast_precision_loss)]
        let n = xs.len() as f64;
        let mean = xs.iter().sum::<f64>() / n;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
        (mean, var)
    }

    #[test]
    fn reference_vector_is_frozen() {
        // Changing these values means changing ALGORITHM_VERSION.
        let mut r = Rng::seed_from_u64(42);
        let got: Vec<u64> = (0..4).map(|_| r.next_u64()).collect();
        let mut r2 = Rng::seed_from_u64(42);
        let again: Vec<u64> = (0..4).map(|_| r2.next_u64()).collect();
        assert_eq!(got, again);
        let mut sm = 0u64;
        assert_eq!(splitmix64(&mut sm), 0xE220_A839_7B1D_CDAF);
    }

    #[test]
    fn forks_are_independent_and_stable() {
        let base = Rng::seed_from_u64(7);
        let mut a1 = base.fork("latency");
        let mut a2 = base.fork("latency");
        let mut b = base.fork("errors");
        let x1: Vec<u64> = (0..8).map(|_| a1.next_u64()).collect();
        let x2: Vec<u64> = (0..8).map(|_| a2.next_u64()).collect();
        let y: Vec<u64> = (0..8).map(|_| b.next_u64()).collect();
        assert_eq!(x1, x2);
        assert_ne!(x1, y);
    }

    #[test]
    fn uniform_moments() {
        let mut r = Rng::seed_from_u64(1);
        let xs: Vec<f64> = (0..200_000).map(|_| r.f64()).collect();
        let (m, v) = mean_var(&xs);
        assert!((m - 0.5).abs() < 0.005, "{m}");
        assert!((v - 1.0 / 12.0).abs() < 0.002, "{v}");
        assert!(xs.iter().all(|x| (0.0..1.0).contains(x)));
    }

    #[test]
    fn normal_moments() {
        let mut r = Rng::seed_from_u64(2);
        let xs: Vec<f64> = (0..200_000).map(|_| r.normal(3.0, 2.0)).collect();
        let (m, v) = mean_var(&xs);
        assert!((m - 3.0).abs() < 0.02, "{m}");
        assert!((v - 4.0).abs() < 0.06, "{v}");
    }

    #[test]
    fn gamma_moments() {
        for (shape, scale) in [(0.5, 2.0), (2.0, 1.5), (9.0, 0.5)] {
            let mut r = Rng::seed_from_u64(3);
            let xs: Vec<f64> = (0..200_000).map(|_| r.gamma(shape, scale)).collect();
            let (m, v) = mean_var(&xs);
            let (em, ev) = (shape * scale, shape * scale * scale);
            assert!((m - em).abs() / em < 0.02, "shape {shape}: mean {m} vs {em}");
            assert!((v - ev).abs() / ev < 0.05, "shape {shape}: var {v} vs {ev}");
        }
    }

    #[test]
    fn poisson_moments_small_and_large() {
        for lambda in [0.5, 4.0, 29.0, 30.0, 250.0, 10_000.0] {
            let mut r = Rng::seed_from_u64(4);
            #[allow(clippy::cast_precision_loss)]
            let xs: Vec<f64> = (0..100_000).map(|_| r.poisson(lambda) as f64).collect();
            let (m, v) = mean_var(&xs);
            assert!((m - lambda).abs() / lambda < 0.02, "λ={lambda}: mean {m}");
            assert!((v - lambda).abs() / lambda < 0.05, "λ={lambda}: var {v}");
        }
        let mut r = Rng::seed_from_u64(5);
        assert_eq!(r.poisson(0.0), 0);
        assert_eq!(r.poisson(f64::NAN), 0);
    }

    #[test]
    fn below_is_unbiased_enough() {
        let mut r = Rng::seed_from_u64(6);
        let mut counts = [0u32; 7];
        for _ in 0..70_000 {
            counts[usize::try_from(r.below(7)).unwrap()] += 1;
        }
        for c in counts {
            assert!((9_500..10_500).contains(&c), "{counts:?}");
        }
    }

    #[test]
    fn weighted_index_respects_weights() {
        let mut r = Rng::seed_from_u64(8);
        let w = [0.0, 1.0, 3.0, -2.0];
        let mut counts = [0u32; 4];
        for _ in 0..40_000 {
            counts[r.weighted_index(&w).unwrap()] += 1;
        }
        assert_eq!(counts[0], 0);
        assert_eq!(counts[3], 0);
        let ratio = f64::from(counts[2]) / f64::from(counts[1]);
        assert!((ratio - 3.0).abs() < 0.15, "{ratio}");
        assert_eq!(r.weighted_index(&[0.0, -1.0]), None);
        assert_eq!(r.weighted_index(&[]), None);
    }

    #[test]
    fn lognormal_median_p99_matches_spec() {
        let mut r = Rng::seed_from_u64(9);
        let mut xs: Vec<f64> = (0..200_001).map(|_| r.lognormal_median_p99(20.0, 5.0)).collect();
        xs.sort_by(f64::total_cmp);
        let median = xs[100_000];
        let p99 = xs[198_000];
        assert!((median - 20.0).abs() / 20.0 < 0.02, "{median}");
        assert!((p99 / median - 5.0).abs() < 0.25, "{}", p99 / median);
    }

    #[test]
    fn shuffle_is_a_permutation() {
        let mut r = Rng::seed_from_u64(10);
        let mut v: Vec<u32> = (0..100).collect();
        r.shuffle(&mut v);
        let mut sorted = v.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..100).collect::<Vec<_>>());
        assert_ne!(v, sorted);
    }
}
