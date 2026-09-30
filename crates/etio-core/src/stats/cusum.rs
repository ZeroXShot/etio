//! Two-sided CUSUM change detection on standardised residuals.
//!
//! Peaks-over-threshold detection catches large excursions; CUSUM catches
//! small *sustained* shifts (a latency that is 1.5σ higher for two minutes),
//! and it also estimates when the shift started, which is what root-cause
//! ranking needs to reason about temporal precedence.

use serde::{Deserialize, Serialize};

/// Two-sided CUSUM with reference value `k` and decision interval `h`, both in
/// units of standard deviations of the input.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cusum {
    k: f64,
    h: f64,
    up: f64,
    down: f64,
    /// Index of the observation at which the upper sum last left zero.
    up_start: u64,
    /// Index of the observation at which the lower sum last left zero.
    down_start: u64,
    index: u64,
}

/// Outcome of a CUSUM update.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CusumState {
    /// The upper sum exceeds `h`: a sustained increase.
    pub alarm_up: bool,
    /// The lower sum exceeds `h`: a sustained decrease.
    pub alarm_down: bool,
    /// Observation index at which the active shift is estimated to have started.
    pub onset: Option<u64>,
}

impl Cusum {
    /// Creates a detector. Typical values are `k = 0.5`, `h = 5`, which detect
    /// a one-sigma shift in about ten observations with an in-control average
    /// run length of roughly 900 observations.
    #[must_use]
    pub const fn new(k: f64, h: f64) -> Self {
        Self { k, h, up: 0.0, down: 0.0, up_start: 0, down_start: 0, index: 0 }
    }

    /// Feeds one standardised residual (non-finite values are skipped).
    pub fn update(&mut self, z: f64) -> CusumState {
        if z.is_finite() {
            if self.up == 0.0 {
                self.up_start = self.index;
            }
            if self.down == 0.0 {
                self.down_start = self.index;
            }
            self.up = (self.up + z - self.k).max(0.0);
            self.down = (self.down - z - self.k).max(0.0);
            self.index += 1;
        }
        let alarm_up = self.up > self.h;
        let alarm_down = self.down > self.h;
        let onset = match (alarm_up, alarm_down) {
            (true, false) => Some(self.up_start),
            (false, true) => Some(self.down_start),
            (true, true) => Some(if self.up >= self.down { self.up_start } else { self.down_start }),
            (false, false) => None,
        };
        CusumState { alarm_up, alarm_down, onset }
    }

    /// Current upper and lower sums.
    #[must_use]
    pub const fn sums(&self) -> (f64, f64) {
        (self.up, self.down)
    }

    /// Number of finite observations processed.
    #[must_use]
    pub const fn observations(&self) -> u64 {
        self.index
    }

    /// Clears both sums, for example after the baseline has been re-learned.
    pub fn reset(&mut self) {
        self.up = 0.0;
        self.down = 0.0;
    }
}

/// Offline single change-point estimate for a mean shift.
///
/// Returns the index `τ` in `1..xs.len()` that maximises the between-segment
/// variance of `xs[..τ]` and `xs[τ..]` (the maximum-likelihood change point
/// for a Gaussian mean shift), restricted to `[lo, hi)`, together with the
/// standardised size of the shift. NaN values are treated as missing.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn mean_shift_point(xs: &[f64], lo: usize, hi: usize) -> Option<(usize, f64)> {
    let n = xs.len();
    let hi = hi.min(n);
    if n < 4 || lo >= hi {
        return None;
    }
    // Prefix sums over finite values.
    let mut sum = vec![0.0; n + 1];
    let mut sq = vec![0.0; n + 1];
    let mut cnt = vec![0usize; n + 1];
    for (i, &x) in xs.iter().enumerate() {
        let (v, c) = if x.is_finite() { (x, 1) } else { (0.0, 0) };
        sum[i + 1] = sum[i] + v;
        sq[i + 1] = sq[i] + v * v;
        cnt[i + 1] = cnt[i] + c;
    }
    let total_n = cnt[n] as f64;
    if total_n < 4.0 {
        return None;
    }
    let total_mean = sum[n] / total_n;
    let total_var = (sq[n] / total_n - total_mean * total_mean).max(0.0);
    let mut best: Option<(usize, f64)> = None;
    for tau in lo.max(2)..hi.min(n - 1) {
        let n1 = cnt[tau] as f64;
        let n2 = total_n - n1;
        if n1 < 2.0 || n2 < 2.0 {
            continue;
        }
        let m1 = sum[tau] / n1;
        let m2 = (sum[n] - sum[tau]) / n2;
        let between = n1 * n2 / total_n * (m2 - m1) * (m2 - m1);
        if best.is_none_or(|(_, b)| between > b) {
            best = Some((tau, between));
        }
    }
    best.map(|(tau, between)| {
        let n1 = cnt[tau] as f64;
        let m1 = sum[tau] / n1;
        let m2 = (sum[n] - sum[tau]) / (total_n - n1);
        let within = (total_var - between / total_n).max(1e-12).sqrt();
        (tau, (m2 - m1) / within)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    #[test]
    fn detects_sustained_shift_and_its_onset() {
        let mut rng = Rng::seed_from_u64(21);
        // h = 8 gives an in-control run length of several thousand steps.
        let mut c = Cusum::new(0.5, 8.0);
        for _ in 0..200 {
            let s = c.update(rng.normal(0.0, 1.0));
            assert!(!s.alarm_up && !s.alarm_down);
        }
        let mut detected = None;
        for i in 0..100 {
            let s = c.update(rng.normal(1.5, 1.0));
            if s.alarm_up {
                detected = Some((i, s.onset.unwrap()));
                break;
            }
        }
        let (delay, onset) = detected.expect("shift detected");
        assert!(delay < 25, "delay {delay}");
        assert!((190..=205).contains(&onset), "onset {onset}");
    }

    #[test]
    fn downward_shift() {
        let mut c = Cusum::new(0.5, 3.0);
        for _ in 0..5 {
            c.update(0.0);
        }
        let mut s = c.update(-2.0);
        for _ in 0..3 {
            s = c.update(-2.0);
        }
        assert!(s.alarm_down);
        assert_eq!(s.onset, Some(5));
        c.reset();
        assert_eq!(c.sums(), (0.0, 0.0));
    }

    #[test]
    fn change_point_of_a_step() {
        let mut rng = Rng::seed_from_u64(22);
        let xs: Vec<f64> =
            (0..300).map(|i| if i < 180 { rng.normal(10.0, 1.0) } else { rng.normal(14.0, 1.0) }).collect();
        let (tau, size) = mean_shift_point(&xs, 0, xs.len()).unwrap();
        assert!((175..=185).contains(&tau), "{tau}");
        assert!(size > 3.0, "{size}");
        assert!(mean_shift_point(&xs[..3], 0, 3).is_none());
    }

    #[test]
    fn change_point_ignores_missing_values() {
        let mut xs = vec![0.0; 50];
        xs.extend(vec![5.0; 50]);
        xs[10] = f64::NAN;
        xs[70] = f64::NAN;
        let (tau, _) = mean_shift_point(&xs, 0, xs.len()).unwrap();
        assert_eq!(tau, 50);
    }
}
