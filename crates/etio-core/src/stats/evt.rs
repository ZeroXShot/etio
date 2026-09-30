//! Extreme value theory: generalised Pareto fitting and the SPOT detector.
//!
//! A fixed threshold ("alert when z > 3") has a false-alarm rate that depends
//! on the unknown tail of each series: heavy-tailed series page constantly,
//! light-tailed ones hide real incidents. The Pickands–Balkema–de Haan theorem
//! says that, above a high enough threshold `t`, the excesses `X − t` of
//! almost any distribution follow a generalised Pareto distribution (GPD).
//! Fitting that GPD lets us place the alarm threshold at a chosen *risk*
//! (probability of exceedance), which is the same for every series.
//!
//! [`Spot`] implements the streaming procedure of Siffer et al.,
//! "Anomaly Detection in Streams with Extreme Value Theory" (KDD 2017), with
//! bounded memory. [`Gpd::fit`] implements maximum-likelihood estimation with
//! Grimshaw's reduction to a one-dimensional root search.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use super::robust;

/// A generalised Pareto distribution for excesses `Y = X − t ≥ 0`.
///
/// `P(Y > y) = (1 + ξ y / σ)^(−1/ξ)`, or `exp(−y / σ)` when `ξ = 0`.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gpd {
    /// Scale parameter `σ > 0`.
    pub sigma: f64,
    /// Shape parameter `ξ` (positive: heavy tail, negative: bounded tail).
    pub xi: f64,
}

const XI_ZERO: f64 = 1e-9;

impl Gpd {
    /// Survival function `P(Y > y)`.
    #[must_use]
    pub fn sf(&self, y: f64) -> f64 {
        if y <= 0.0 {
            return 1.0;
        }
        if self.xi.abs() < XI_ZERO {
            return (-y / self.sigma).exp();
        }
        let base = 1.0 + self.xi * y / self.sigma;
        if base <= 0.0 {
            // Beyond the upper end point of a bounded tail.
            return 0.0;
        }
        base.powf(-1.0 / self.xi)
    }

    /// The excess `y` such that `P(Y > y) = p`, for `p` in `(0, 1]`.
    #[must_use]
    pub fn isf(&self, p: f64) -> f64 {
        let p = p.clamp(f64::MIN_POSITIVE, 1.0);
        if self.xi.abs() < XI_ZERO { -self.sigma * p.ln() } else { self.sigma / self.xi * (p.powf(-self.xi) - 1.0) }
    }

    /// Log-likelihood of a sample of excesses.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn log_likelihood(&self, ys: &[f64]) -> f64 {
        let n = ys.len() as f64;
        if self.sigma <= 0.0 {
            return f64::NEG_INFINITY;
        }
        if self.xi.abs() < XI_ZERO {
            return -n * self.sigma.ln() - ys.iter().sum::<f64>() / self.sigma;
        }
        let mut acc = 0.0;
        for &y in ys {
            let t = 1.0 + self.xi * y / self.sigma;
            if t <= 0.0 {
                return f64::NEG_INFINITY;
            }
            acc += t.ln();
        }
        -n * self.sigma.ln() - (1.0 + 1.0 / self.xi) * acc
    }

    /// Maximum-likelihood fit to strictly positive excesses.
    ///
    /// Grimshaw (1993) showed that the MLE satisfies `ξ = v(θ) − 1`,
    /// `σ = ξ / θ` where `θ` is a root of `u(θ) v(θ) = 1` with
    /// `u(θ) = mean(1 / (1 + θ y))` and `v(θ) = 1 + mean(ln(1 + θ y))`.
    /// The roots are bracketed on a grid and refined by bisection, and the
    /// candidate with the highest likelihood wins, the exponential model
    /// (`ξ = 0`) included. The search is deterministic.
    ///
    /// Returns `None` for fewer than two excesses or non-positive data.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn fit(ys: &[f64]) -> Option<Self> {
        if ys.len() < 2 || ys.iter().any(|y| !(y.is_finite() && *y > 0.0)) {
            return None;
        }
        let n = ys.len() as f64;
        let mean = ys.iter().sum::<f64>() / n;
        let ymin = ys.iter().copied().fold(f64::INFINITY, f64::min);
        let ymax = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);

        let exponential = Self { sigma: mean, xi: 0.0 };
        let mut best = (exponential.log_likelihood(ys), exponential);
        if (ymax - ymin) <= 1e-12 * ymax {
            return Some(exponential);
        }

        let w = |theta: f64| -> f64 {
            let mut u = 0.0;
            let mut v = 0.0;
            for &y in ys {
                let s = 1.0 + theta * y;
                u += 1.0 / s;
                v += s.ln();
            }
            (u / n) * (1.0 + v / n) - 1.0
        };
        let candidate = |theta: f64| -> Option<Self> {
            let xi = ys.iter().map(|&y| (1.0 + theta * y).ln()).sum::<f64>() / n;
            let sigma = xi / theta;
            (sigma.is_finite() && sigma > 0.0).then_some(Self { sigma, xi })
        };

        let eps = 1e-8 / mean;
        let lower = -1.0 / ymax + eps;
        let upper = (2.0 * (mean - ymin) / (ymin * ymin)).min(1e12 / mean);
        let mut intervals = Vec::with_capacity(2);
        if lower < -eps {
            intervals.push((lower, -eps, false));
        }
        if upper > eps {
            intervals.push((eps, upper, true));
        }

        const GRID: usize = 48;
        for (a, b, geometric) in intervals {
            let point = |i: usize| -> f64 {
                let f = i as f64 / GRID as f64;
                if geometric { a * (b / a).powf(f) } else { a + (b - a) * f }
            };
            let mut prev_t = point(0);
            let mut prev_w = w(prev_t);
            for i in 1..=GRID {
                let t = point(i);
                let wt = w(t);
                if prev_w.is_finite() && wt.is_finite() && (prev_w == 0.0 || prev_w.signum() != wt.signum()) {
                    let root = bisect(&w, prev_t, t, prev_w);
                    if let Some(c) = candidate(root) {
                        let ll = c.log_likelihood(ys);
                        if ll > best.0 {
                            best = (ll, c);
                        }
                    }
                }
                prev_t = t;
                prev_w = wt;
            }
        }
        Some(best.1)
    }
}

/// Bisection for a sign change of `f` on `[a, b]` given `f(a)`.
fn bisect(f: &impl Fn(f64) -> f64, mut a: f64, mut b: f64, mut fa: f64) -> f64 {
    for _ in 0..80 {
        let mid = 0.5 * (a + b);
        let fm = f(mid);
        if fm == 0.0 || (b - a).abs() <= 1e-15 * mid.abs().max(1e-300) {
            return mid;
        }
        if fa.signum() == fm.signum() {
            a = mid;
            fa = fm;
        } else {
            b = mid;
        }
    }
    0.5 * (a + b)
}

/// Configuration of a [`Spot`] detector.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpotConfig {
    /// Target probability that a normal observation exceeds the alarm threshold.
    pub risk: f64,
    /// Quantile of the calibration sample used as the initial threshold `t`.
    pub init_quantile: f64,
    /// Number of observations collected before the first fit.
    pub calibration: usize,
    /// Maximum number of excesses kept for fitting (oldest are forgotten).
    pub max_peaks: usize,
    /// Observation count at which the exceedance-rate counters are halved, so
    /// that the rate estimate follows slow drifts.
    pub max_observations: u64,
    /// Minimum number of excesses needed to trust a GPD fit. Below it the
    /// detector falls back to a Gaussian tail on the observations.
    pub min_peaks: usize,
}

impl Default for SpotConfig {
    fn default() -> Self {
        Self {
            risk: 1e-4,
            init_quantile: 0.98,
            calibration: 250,
            max_peaks: 400,
            max_observations: 50_000,
            min_peaks: 10,
        }
    }
}

/// What a [`Spot`] detector made of an observation.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum SpotVerdict {
    /// Still collecting calibration data; no decision is possible.
    Calibrating,
    /// Below the initial threshold.
    Normal {
        /// Estimated probability of an observation at least this large.
        p: f64,
    },
    /// Between the initial threshold and the alarm threshold; used to refine the tail model.
    Peak {
        /// Estimated probability of an observation at least this large.
        p: f64,
    },
    /// Above the alarm threshold. Not added to the tail model.
    Alarm {
        /// Estimated probability of an observation at least this large.
        p: f64,
    },
}

impl SpotVerdict {
    /// The tail probability, if a decision was made.
    #[must_use]
    pub const fn p(&self) -> Option<f64> {
        match *self {
            Self::Calibrating => None,
            Self::Normal { p } | Self::Peak { p } | Self::Alarm { p } => Some(p),
        }
    }

    /// Whether this is an alarm.
    #[must_use]
    pub const fn is_alarm(&self) -> bool {
        matches!(self, Self::Alarm { .. })
    }
}

/// Streaming peaks-over-threshold detector (upper tail).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spot {
    cfg: SpotConfig,
    calibration: Vec<f64>,
    /// Initial threshold `t`; `None` while calibrating.
    t: Option<f64>,
    peaks: VecDeque<f64>,
    /// Observations accounted for in the exceedance rate.
    n: u64,
    /// Exceedances of `t` among those observations.
    n_t: u64,
    gpd: Option<Gpd>,
    /// Alarm threshold `z_q`.
    z_q: f64,
    /// Gaussian fallback parameters (mean, std) of the calibration sample.
    fallback: (f64, f64),
    /// The calibration sample was dominated by one repeated value (zero-
    /// inflated counts, idle series). Without a tail model such a series
    /// gets empirical p-values instead of Gaussian ones.
    #[serde(default)]
    degenerate: bool,
}

impl Spot {
    /// Creates a detector.
    #[must_use]
    pub fn new(cfg: SpotConfig) -> Self {
        Self {
            cfg,
            calibration: Vec::with_capacity(cfg.calibration),
            t: None,
            peaks: VecDeque::new(),
            n: 0,
            n_t: 0,
            gpd: None,
            z_q: f64::INFINITY,
            fallback: (0.0, 1.0),
            degenerate: false,
        }
    }

    /// Creates a detector already calibrated on `sample`.
    #[must_use]
    pub fn calibrated(cfg: SpotConfig, sample: &[f64]) -> Self {
        let mut s = Self::new(SpotConfig { calibration: sample.len().max(1), ..cfg });
        for &x in sample {
            s.observe(x);
        }
        s
    }

    /// The configuration.
    #[must_use]
    pub const fn config(&self) -> &SpotConfig {
        &self.cfg
    }

    /// Whether calibration is complete.
    #[must_use]
    pub const fn is_calibrated(&self) -> bool {
        self.t.is_some()
    }

    /// Current alarm threshold (infinite while calibrating).
    #[must_use]
    pub const fn threshold(&self) -> f64 {
        self.z_q
    }

    /// Current tail model, if one could be fitted.
    #[must_use]
    pub const fn model(&self) -> Option<Gpd> {
        self.gpd
    }

    /// Estimated probability of observing a value at least `x`, without
    /// updating the detector. `None` while calibrating.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn tail_probability(&self, x: f64) -> Option<f64> {
        let t = self.t?;
        let rate = (self.n_t as f64 / self.n.max(1) as f64).clamp(1e-12, 1.0);
        if self.gpd.is_none() && self.degenerate {
            return Some(self.empirical_sf(x));
        }
        if x <= t {
            return Some(rate.max(self.gaussian_sf(x)).min(1.0));
        }
        Some(match self.gpd {
            Some(g) => (rate * g.sf(x - t)).max(f64::MIN_POSITIVE),
            None => self.gaussian_sf(x).max(f64::MIN_POSITIVE),
        })
    }

    /// Conformal p-value `(1 + #{past values ≥ x}) / (n + 1)`, using the
    /// retained excesses for the values above the threshold. Distribution
    /// free; its floor `1 / (n + 1)` is the honest limit of what `n`
    /// observations can say about a tail.
    #[allow(clippy::cast_precision_loss)]
    fn empirical_sf(&self, x: f64) -> f64 {
        let Some(t) = self.t else { return 1.0 };
        if x <= t {
            // Only the excesses are retained: nothing below the threshold can
            // be called surprising.
            return 1.0;
        }
        let n = self.n.max(1) as f64;
        let at_least = self.peaks.iter().filter(|&&y| y >= x - t).count() as f64;
        ((1.0 + at_least) / (n + 1.0)).min(1.0)
    }

    fn gaussian_sf(&self, x: f64) -> f64 {
        let (mean, std) = self.fallback;
        super::special::normal_sf((x - mean) / std.max(1e-12))
    }

    /// Processes one observation. Non-finite values are ignored.
    pub fn observe(&mut self, x: f64) -> SpotVerdict {
        if !x.is_finite() {
            return match self.t {
                None => SpotVerdict::Calibrating,
                Some(_) => SpotVerdict::Normal { p: 1.0 },
            };
        }
        let Some(t) = self.t else {
            self.calibration.push(x);
            if self.calibration.len() >= self.cfg.calibration {
                self.finish_calibration();
            }
            return SpotVerdict::Calibrating;
        };
        let p = self.tail_probability(x).unwrap_or(1.0);
        if x > self.z_q {
            // Alarms count towards the exceedance rate but are kept out of the
            // tail model: an anomaly must not teach the detector that
            // anomalies are normal. Short, isolated excursions can be fed
            // back later with [`Spot::learn`].
            self.bump_counts(true);
            return SpotVerdict::Alarm { p };
        }
        self.bump_counts(x > t);
        if x > t {
            self.push_peak(x - t);
            self.refit();
            SpotVerdict::Peak { p }
        } else {
            SpotVerdict::Normal { p }
        }
    }

    /// Adds a previously observed alarm value to the tail model.
    ///
    /// Excluding every alarm from the model censors the most extreme
    /// observations, which biases the estimated tail towards being too light
    /// and inflates the false-alarm rate on heavy-tailed series. Callers that
    /// can tell, after the fact, that an alarm was an isolated excursion
    /// rather than the start of an anomaly should feed it back here. The
    /// observation must already have been passed to [`Spot::observe`].
    pub fn learn(&mut self, x: f64) {
        let Some(t) = self.t else { return };
        if x.is_finite() && x > t {
            self.push_peak(x - t);
            self.refit();
        }
    }

    fn bump_counts(&mut self, exceeded: bool) {
        self.n += 1;
        if exceeded {
            self.n_t += 1;
        }
        if self.n >= self.cfg.max_observations {
            self.n /= 2;
            self.n_t /= 2;
        }
    }

    fn push_peak(&mut self, y: f64) {
        if self.peaks.len() == self.cfg.max_peaks {
            self.peaks.pop_front();
        }
        self.peaks.push_back(y);
    }

    #[allow(clippy::cast_precision_loss)]
    fn finish_calibration(&mut self) {
        let sample = std::mem::take(&mut self.calibration);
        let summary = robust::RobustSummary::of(&sample);
        let (mean, std) = summary.map_or((0.0, 1.0), |s| (s.mean, s.std_dev));
        self.fallback = (mean, std);
        let median = summary.map_or(0.0, |s| s.median);
        let ties = sample.iter().filter(|&&x| x.total_cmp(&median).is_eq()).count();
        self.degenerate = 2 * ties > sample.len();
        let t = robust::quantile(&sample, self.cfg.init_quantile);
        self.t = Some(t);
        self.n = sample.len() as u64;
        self.n_t = 0;
        for &x in &sample {
            if x > t {
                self.n_t += 1;
                self.push_peak(x - t);
            }
        }
        self.refit();
    }

    #[allow(clippy::cast_precision_loss)]
    fn refit(&mut self) {
        let Some(t) = self.t else { return };
        let rate = self.n_t as f64 / self.n.max(1) as f64;
        let ys: Vec<f64> = self.peaks.iter().copied().collect();
        self.gpd = if ys.len() >= self.cfg.min_peaks { Gpd::fit(&ys) } else { None };
        self.z_q = match self.gpd {
            Some(g) if rate > 0.0 => {
                let p = (self.cfg.risk / rate).min(1.0);
                t + g.isf(p)
            }
            // A degenerate sample cannot certify the configured risk: no
            // extreme alarms until enough excesses exist to fit a tail.
            _ if self.degenerate => f64::INFINITY,
            _ => {
                let (mean, std) = self.fallback;
                let z = super::special::normal_quantile(1.0 - self.cfg.risk);
                (mean + z * std.max(1e-12)).max(t)
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    fn gpd_sample(rng: &mut Rng, sigma: f64, xi: f64, n: usize) -> Vec<f64> {
        (0..n).map(|_| Gpd { sigma, xi }.isf(rng.f64_open0())).collect()
    }

    #[test]
    fn sf_and_isf_are_inverse() {
        for g in [Gpd { sigma: 2.0, xi: 0.3 }, Gpd { sigma: 1.0, xi: 0.0 }, Gpd { sigma: 1.5, xi: -0.2 }] {
            for p in [0.9, 0.5, 0.1, 1e-3, 1e-6] {
                let y = g.isf(p);
                assert!((g.sf(y) - p).abs() / p < 1e-9, "{g:?} p={p}");
            }
        }
        let bounded = Gpd { sigma: 1.0, xi: -0.5 };
        assert!(bounded.sf(3.0).abs() < 1e-15, "beyond the end point");
    }

    #[test]
    fn fit_recovers_parameters() {
        let mut rng = Rng::seed_from_u64(11);
        for (sigma, xi) in [(1.0, 0.25), (3.0, 0.0), (2.0, -0.2), (0.5, 0.5)] {
            let ys = gpd_sample(&mut rng, sigma, xi, 5_000);
            let g = Gpd::fit(&ys).unwrap();
            assert!((g.xi - xi).abs() < 0.06, "xi {} vs {xi}", g.xi);
            assert!((g.sigma - sigma).abs() / sigma < 0.08, "sigma {} vs {sigma}", g.sigma);
        }
    }

    #[test]
    fn fit_rejects_bad_input() {
        assert!(Gpd::fit(&[1.0]).is_none());
        assert!(Gpd::fit(&[1.0, -1.0]).is_none());
        assert!(Gpd::fit(&[1.0, f64::NAN]).is_none());
        let g = Gpd::fit(&[2.0, 2.0, 2.0]).unwrap();
        assert!((g.sigma - 2.0).abs() < 1e-12);
    }

    #[test]
    fn spot_false_alarm_rate_matches_risk() {
        // Heavy-tailed noise: a fixed 3-sigma rule would fire constantly.
        let mut rng = Rng::seed_from_u64(12);
        let cfg = SpotConfig { risk: 1e-3, calibration: 2_000, max_peaks: 1_000, ..SpotConfig::default() };
        let mut spot = Spot::new(cfg);
        let draw = |rng: &mut Rng| {
            let n = rng.standard_normal();
            let chi = rng.gamma(1.5, 2.0); // Student-t with 3 d.o.f.
            n / (chi / 3.0).sqrt()
        };
        let mut alarms = 0u32;
        let trials = 200_000u32;
        for _ in 0..cfg.calibration {
            spot.observe(draw(&mut rng));
        }
        for _ in 0..trials {
            let x = draw(&mut rng);
            if spot.observe(x).is_alarm() {
                alarms += 1;
                // In-control data: every alarm is an isolated excursion.
                spot.learn(x);
            }
        }
        let rate = f64::from(alarms) / f64::from(trials);
        assert!(rate < 2e-3 && rate > 4e-4, "false alarm rate {rate}");
    }

    #[test]
    fn spot_flags_a_level_shift() {
        let mut rng = Rng::seed_from_u64(13);
        let mut spot = Spot::new(SpotConfig::default());
        for _ in 0..1_000 {
            spot.observe(rng.normal(0.0, 1.0));
        }
        assert!(spot.is_calibrated());
        let verdict = spot.observe(8.0);
        assert!(verdict.is_alarm(), "{verdict:?} threshold {}", spot.threshold());
        assert!(verdict.p().unwrap() < 1e-6);
    }

    #[test]
    fn zero_inflated_series_get_honest_p_values() {
        // Mostly zeros with rare events: without a tail model, an event is
        // "rare", not "impossible".
        let mut rng = Rng::seed_from_u64(14);
        let sample: Vec<f64> = (0..200).map(|_| if rng.chance(0.03) { 5.0 } else { 0.0 }).collect();
        let spot = Spot::calibrated(SpotConfig::default(), &sample);
        assert!(spot.threshold().is_infinite(), "no certified threshold");
        let p = spot.tail_probability(5.0).unwrap();
        assert!(p >= 0.01, "an ordinary event is not surprising: {p}");
        let p_new = spot.tail_probability(50.0).unwrap();
        assert!((p_new - 1.0 / 201.0).abs() < 1e-12, "{p_new}");
    }

    #[test]
    fn calibrating_until_enough_data() {
        let mut spot = Spot::new(SpotConfig { calibration: 3, ..SpotConfig::default() });
        assert_eq!(spot.observe(1.0), SpotVerdict::Calibrating);
        assert_eq!(spot.observe(f64::NAN), SpotVerdict::Calibrating);
        assert_eq!(spot.observe(2.0), SpotVerdict::Calibrating);
        assert!(spot.tail_probability(3.0).is_none());
        assert_eq!(spot.observe(3.0), SpotVerdict::Calibrating);
        assert!(spot.is_calibrated());
        assert!(spot.tail_probability(3.0).is_some());
    }
}
