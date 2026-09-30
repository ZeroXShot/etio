//! Streaming anomaly detection for a single series.
//!
//! Each series gets a [`SeriesDetector`] that sees one value per aggregation
//! window. The detector combines three ideas:
//!
//! 1. **A robust, self-updating baseline.** The median and MAD of a sliding
//!    reference window turn raw values into standardised residuals `z`. The
//!    scale has relative floors so that flat series (an error rate that is
//!    always zero) do not produce infinite scores on their first non-zero
//!    value, while staying unit-free.
//! 2. **Extreme-value thresholds.** The harmful part of `z` is fed to a
//!    [`Spot`] detector, so every series alarms at the same configured risk
//!    regardless of how heavy its tails are.
//! 3. **Sustained-shift detection.** A CUSUM on `z` catches moderate shifts
//!    that never cross the extreme threshold, and estimates their onset.
//!
//! While a series is anomalous its baseline is *frozen*, so the anomaly is
//! not absorbed into "normal". If the new level persists longer than
//! `freeze_max_points`, the detector accepts it as the new normal (a
//! deployment that changed latency for good is not an incident forever).
//! Short episodes are fed back into the tail model after they end, which
//! removes the censoring bias described in [`Spot::learn`].

use etio_core::Direction;
use etio_core::stats::MAD_TO_SIGMA;
use etio_core::stats::cusum::Cusum;
use etio_core::stats::evt::{Spot, SpotConfig, SpotVerdict};
use etio_core::stats::online::Ewm;
use etio_core::stats::window::SortedWindow;
use serde::{Deserialize, Serialize};

/// Tunables of a [`SeriesDetector`]. Point counts are in aggregation windows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectorConfig {
    /// Size of the sliding reference window.
    pub baseline_points: usize,
    /// Points needed before any value is scored.
    pub min_baseline: usize,
    /// Relative noise floor: the scale is at least this fraction of the
    /// baseline median and of the long-run magnitude of the series.
    pub rel_floor: f64,
    /// Extreme-value detector settings (applied to harmful `z`).
    pub spot: SpotConfig,
    /// Harmful `z` above which a value alarms while SPOT is still calibrating.
    pub fallback_threshold: f64,
    /// CUSUM reference value, in standard deviations.
    pub cusum_k: f64,
    /// CUSUM decision interval, in standard deviations.
    pub cusum_h: f64,
    /// Residuals are winsorised at `±cusum_clip` before entering the CUSUM
    /// (a Huber-type robust CUSUM). Without the clip, one extreme outlier in
    /// a sparse series (z in the thousands) would hold the CUSUM in alarm for
    /// thousands of windows, since the sum only decays by `k` per window.
    pub cusum_clip: f64,
    /// Longest time the baseline stays frozen during an anomaly.
    pub freeze_max_points: usize,
    /// Episodes up to this length are considered isolated excursions and are
    /// fed back into the tail model when they end.
    pub learnable_run: usize,
    /// Upper bound on the reported surprise (`−log10 p`).
    pub max_surprise: f64,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            baseline_points: 240,
            min_baseline: 30,
            rel_floor: 0.02,
            spot: SpotConfig { calibration: 120, ..SpotConfig::default() },
            fallback_threshold: 6.0,
            cusum_k: 1.0,
            cusum_h: 12.0,
            cusum_clip: 4.0,
            freeze_max_points: 180,
            learnable_run: 2,
            max_surprise: 30.0,
        }
    }
}

/// Health state of a series after an observation.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeriesState {
    /// Not enough history to judge.
    Warmup,
    /// Within its normal range.
    Normal,
    /// Anomalous in its harmful direction.
    Anomalous,
    /// No value in this window.
    Missing,
}

/// The detector's verdict for one value.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    /// State after this value.
    pub state: SeriesState,
    /// Signed standardised residual.
    pub z: f64,
    /// Estimated probability of a harmful deviation at least this large.
    pub p: f64,
    /// `−log10 p`, capped: an additive, comparable measure of surprise.
    pub surprise: f64,
    /// Baseline median the value was compared against.
    pub baseline: f64,
    /// For anomalous values, the number of points since the episode began
    /// (0 on the first anomalous point), accounting for the CUSUM onset.
    pub episode_age: u32,
}

impl Observation {
    const fn missing() -> Self {
        Self { state: SeriesState::Missing, z: f64::NAN, p: 1.0, surprise: 0.0, baseline: f64::NAN, episode_age: 0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Episode {
    /// Points since the episode started.
    age: u32,
    /// How many points before the first alarm the CUSUM places the onset.
    lead: u32,
    /// Harmful `z` values that raised SPOT alarms (for deferred learning).
    alarm_values: Vec<f64>,
    /// Raw values seen during the episode (for baseline catch-up).
    values: Vec<f64>,
}

/// Streaming anomaly detector for one series.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeriesDetector {
    cfg: DetectorConfig,
    direction: Direction,
    baseline: SortedWindow,
    magnitude: Ewm,
    spot: Spot,
    cusum: Cusum,
    episode: Option<Episode>,
    observed: u64,
}

impl SeriesDetector {
    /// Creates a detector for a series whose harmful direction is `direction`.
    #[must_use]
    pub fn new(cfg: DetectorConfig, direction: Direction) -> Self {
        Self {
            baseline: SortedWindow::new(cfg.baseline_points.max(1)),
            magnitude: Ewm::with_half_life(cfg.baseline_points.max(1) as f64),
            spot: Spot::new(cfg.spot),
            cusum: Cusum::new(cfg.cusum_k, cfg.cusum_h),
            episode: None,
            observed: 0,
            direction,
            cfg,
        }
    }

    /// The harmful direction of the series.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// Whether the series is currently in an anomalous episode.
    #[must_use]
    pub const fn is_anomalous(&self) -> bool {
        self.episode.is_some()
    }

    /// Number of finite values processed.
    #[must_use]
    pub const fn observed(&self) -> u64 {
        self.observed
    }

    /// Conformal p-value of `x` against the baseline window, in the harmful
    /// direction: `(1 + #{baseline values at least as bad}) / (n + 1)`.
    /// Used for degenerate baselines (more than half of the values equal),
    /// where no parametric tail model is credible.
    #[allow(clippy::cast_precision_loss)]
    fn conformal_p(&self, x: f64) -> f64 {
        let sorted = self.baseline.sorted();
        let n = sorted.len() as f64;
        let at_least = (sorted.len() - sorted.partition_point(|v| *v < x)) as f64;
        let at_most = sorted.partition_point(|v| *v <= x) as f64;
        let p = |k: f64| (1.0 + k) / (n + 1.0);
        match self.direction {
            Direction::Up => p(at_least),
            Direction::Down => p(at_most),
            Direction::Both => (2.0 * p(at_least.min(at_most))).min(1.0),
        }
    }

    /// Current noise scale used to standardise residuals.
    fn scale(&self, median: f64) -> f64 {
        let n = self.baseline.len();
        let mad = self.baseline.mad() * MAD_TO_SIGMA;
        let iqr = self.baseline.iqr() * etio_core::stats::IQR_TO_SIGMA;
        // Sparse series (mostly one value, occasional events) have MAD = IQR
        // = 0; their standard deviation still describes their fluctuations,
        // so an ordinary event is not scored as a thousand-sigma outlier.
        let sparse = if mad.max(iqr) <= 0.0 { self.baseline.std_dev() } else { 0.0 };
        let rel = self.cfg.rel_floor * median.abs().max(self.magnitude.mean().abs());
        let s = mad.max(iqr).max(if sparse.is_finite() { sparse } else { 0.0 }).max(rel);
        if s > 0.0 && s.is_finite() {
            s
        } else if n > 0 {
            // A series that has been exactly zero forever: any non-zero value
            // is a change of unknown scale. Use a unit scale so the first
            // deviation is scored by its magnitude, and let SPOT calibrate.
            1.0
        } else {
            f64::NAN
        }
    }

    /// Processes the value of the next window. `NaN` means "no data".
    pub fn observe(&mut self, x: f64) -> Observation {
        if !x.is_finite() {
            return Observation::missing();
        }
        self.observed += 1;

        if self.baseline.len() < self.cfg.min_baseline {
            self.magnitude.push(x.abs());
            self.baseline.push(x);
            let median = self.baseline.median();
            return Observation {
                state: SeriesState::Warmup,
                z: 0.0,
                p: 1.0,
                surprise: 0.0,
                baseline: median,
                episode_age: 0,
            };
        }

        let median = self.baseline.median();
        let scale = self.scale(median);
        // A baseline that never varied has no noise scale at all: any
        // departure from its constant value is an *event*. Events enter the
        // detectors at the clip value, so one of them is not an alarm but a
        // sustained run accumulates in the CUSUM and is detected within a
        // few windows, whatever the units of the series.
        let constant = self.baseline.std_dev() == 0.0 && self.magnitude.mean().abs() < f64::MIN_POSITIVE.sqrt()
            || self.baseline.sorted().first() == self.baseline.sorted().last();
        let z = if constant {
            let d = x - median;
            if d == 0.0 { 0.0 } else { d.signum() * self.cfg.cusum_clip }
        } else {
            (x - median) / scale
        };
        let harmful = self.direction.harmful(z);

        let verdict = self.spot.observe(harmful);
        // With a degenerate baseline (zero-inflated counts, idle series) a
        // parametric p-value would call any event impossible. The conformal
        // p-value calls it exactly as rare as the baseline says; only a
        // sustained run (the CUSUM) can then make the series anomalous.
        let degenerate = self.baseline.mad() == 0.0 && self.baseline.iqr() == 0.0;
        let p = if degenerate {
            self.conformal_p(x)
        } else {
            match verdict {
                SpotVerdict::Calibrating => etio_core::stats::special::normal_sf(harmful),
                other => other.p().unwrap_or(1.0),
            }
        };
        let extreme = !degenerate
            && match verdict {
                SpotVerdict::Calibrating => harmful > self.cfg.fallback_threshold,
                SpotVerdict::Alarm { .. } => true,
                _ => false,
            };

        // The CUSUM sees the winsorised residual, projected on the harmful direction.
        let clipped = z.clamp(-self.cfg.cusum_clip, self.cfg.cusum_clip);
        let projected = match self.direction {
            Direction::Up | Direction::Both => clipped,
            Direction::Down => -clipped,
        };
        let c = self.cusum.update(projected);
        let sustained = match self.direction {
            Direction::Up | Direction::Down => c.alarm_up,
            Direction::Both => c.alarm_up || c.alarm_down,
        };

        let anomalous = extreme || sustained;
        let surprise = (-p.max(1e-300).log10()).clamp(0.0, self.cfg.max_surprise);

        if anomalous {
            let lead = c
                .onset
                .map_or(0, |onset| u32::try_from(self.cusum.observations().saturating_sub(onset + 1)).unwrap_or(0));
            // The onset lead is fixed when the episode opens: a CUSUM
            // detection points back to where the shift began.
            let ep = self.episode.get_or_insert_with(|| Episode {
                age: 0,
                lead,
                alarm_values: Vec::new(),
                values: Vec::new(),
            });
            if matches!(verdict, SpotVerdict::Alarm { .. }) {
                ep.alarm_values.push(harmful);
            }
            ep.values.push(x);
            let episode_age = ep.age + ep.lead;
            ep.age += 1;

            // Accept a persistent new level as the new normal.
            if ep.age as usize > self.cfg.freeze_max_points {
                let values = std::mem::take(&mut ep.values);
                self.episode = None;
                self.baseline.clear();
                for v in values {
                    self.baseline.push(v);
                }
                self.cusum.reset();
            }
            return Observation { state: SeriesState::Anomalous, z, p, surprise, baseline: median, episode_age };
        }

        if let Some(ep) = self.episode.take() {
            // A short episode was most likely an excursion of a heavy tail:
            // teach the tail model and the baseline about it.
            if ep.age as usize <= self.cfg.learnable_run {
                for a in ep.alarm_values {
                    self.spot.learn(a);
                }
                for v in ep.values {
                    self.baseline.push(v);
                }
            }
            self.cusum.reset();
        }
        // The magnitude only learns from normal data, like the baseline.
        self.magnitude.push(x.abs());
        self.baseline.push(x);
        Observation { state: SeriesState::Normal, z, p, surprise, baseline: median, episode_age: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use etio_core::rng::Rng;

    fn run(det: &mut SeriesDetector, xs: impl IntoIterator<Item = f64>) -> Vec<Observation> {
        xs.into_iter().map(|x| det.observe(x)).collect()
    }

    #[test]
    fn warms_up_then_stays_quiet_on_noise() {
        let mut rng = Rng::seed_from_u64(1);
        let mut det = SeriesDetector::new(DetectorConfig::default(), Direction::Up);
        let obs = run(&mut det, (0..3_000).map(|_| rng.lognormal(3.0, 0.2)));
        assert_eq!(obs[0].state, SeriesState::Warmup);
        let alarms = obs.iter().filter(|o| o.state == SeriesState::Anomalous).count();
        assert!(alarms < 15, "{alarms} alarms on stationary noise");
    }

    #[test]
    fn detects_latency_increase_quickly() {
        let mut rng = Rng::seed_from_u64(2);
        let mut det = SeriesDetector::new(DetectorConfig::default(), Direction::Up);
        run(&mut det, (0..600).map(|_| rng.normal(100.0, 5.0)));
        let obs = run(&mut det, (0..20).map(|_| rng.normal(160.0, 5.0)));
        assert_eq!(obs[0].state, SeriesState::Anomalous);
        assert!(obs[0].surprise > 4.0, "{}", obs[0].surprise);
        assert!(obs.iter().all(|o| o.state == SeriesState::Anomalous));
    }

    #[test]
    fn ignores_harmless_direction() {
        let mut rng = Rng::seed_from_u64(3);
        let mut det = SeriesDetector::new(DetectorConfig::default(), Direction::Up);
        run(&mut det, (0..600).map(|_| rng.normal(100.0, 5.0)));
        let obs = run(&mut det, (0..20).map(|_| rng.normal(40.0, 5.0)));
        assert!(obs.iter().all(|o| o.state != SeriesState::Anomalous));
    }

    #[test]
    fn traffic_drop_is_anomalous_for_two_sided_series() {
        let mut rng = Rng::seed_from_u64(4);
        let mut det = SeriesDetector::new(DetectorConfig::default(), Direction::Both);
        run(&mut det, (0..600).map(|_| rng.normal(50.0, 3.0)));
        assert_eq!(det.observe(0.0).state, SeriesState::Anomalous);
    }

    #[test]
    fn flat_zero_series_flags_sustained_errors_not_single_ones() {
        let mut det = SeriesDetector::new(DetectorConfig::default(), Direction::Up);
        run(&mut det, std::iter::repeat_n(0.0, 400));
        // One error in a series that never had any is rare, not an alarm.
        assert_eq!(det.observe(0.3).state, SeriesState::Normal);
        run(&mut det, std::iter::repeat_n(0.0, 20));
        // A sustained error rate is detected within a few windows.
        let obs = run(&mut det, std::iter::repeat_n(0.3, 10));
        let first = obs.iter().position(|o| o.state == SeriesState::Anomalous).expect("detected");
        assert!(first <= 5, "detected after {first} windows");
        assert!(obs[first].z.is_finite());
    }

    #[test]
    fn sustained_small_shift_is_caught_with_onset() {
        let mut rng = Rng::seed_from_u64(5);
        let mut det = SeriesDetector::new(DetectorConfig::default(), Direction::Up);
        run(&mut det, (0..800).map(|_| rng.normal(10.0, 1.0)));
        // A 2.5 sigma shift rarely crosses the extreme threshold alone.
        let obs = run(&mut det, (0..40).map(|_| rng.normal(12.5, 1.0)));
        let first = obs.iter().position(|o| o.state == SeriesState::Anomalous).expect("detected");
        assert!(first < 12, "detected after {first} points");
        let age = obs[first].episode_age as usize;
        assert!(age + 2 >= first, "onset should point back to the shift: age {age}, first {first}");
    }

    #[test]
    fn an_isolated_outlier_does_not_hold_the_alarm() {
        // A sparse series: zeros with a rare tiny blip, then one event.
        let mut det = SeriesDetector::new(DetectorConfig::default(), Direction::Up);
        let mut xs = vec![0.0; 300];
        xs[40] = 0.001;
        run(&mut det, xs);
        // The event is scored, but a single one neither alarms nor lingers.
        let first = det.observe(0.5);
        assert!(first.z > 3.0);
        let after = run(&mut det, std::iter::repeat_n(0.0, 10));
        assert!(
            after.iter().all(|o| o.state == SeriesState::Normal),
            "{:?}",
            after.iter().map(|o| o.state).collect::<Vec<_>>()
        );
    }

    #[test]
    fn persistent_level_becomes_new_normal() {
        let mut rng = Rng::seed_from_u64(6);
        let cfg = DetectorConfig { freeze_max_points: 50, ..DetectorConfig::default() };
        let mut det = SeriesDetector::new(cfg, Direction::Up);
        run(&mut det, (0..600).map(|_| rng.normal(100.0, 5.0)));
        let obs = run(&mut det, (0..300).map(|_| rng.normal(200.0, 5.0)));
        let tail_alarms = obs[150..].iter().filter(|o| o.state == SeriesState::Anomalous).count();
        assert!(tail_alarms < 10, "{tail_alarms}");
    }

    #[test]
    fn missing_values_do_not_advance_state() {
        let mut det = SeriesDetector::new(DetectorConfig::default(), Direction::Up);
        let o = det.observe(f64::NAN);
        assert_eq!(o.state, SeriesState::Missing);
        assert_eq!(det.observed(), 0);
    }

    #[test]
    fn detector_state_round_trips_through_serde() {
        let mut rng = Rng::seed_from_u64(7);
        let mut det = SeriesDetector::new(DetectorConfig::default(), Direction::Up);
        run(&mut det, (0..500).map(|_| rng.normal(10.0, 1.0)));
        // Snapshots use a binary format: it is exact for every f64, infinities included.
        let bytes = postcard::to_stdvec(&det).unwrap();
        let mut restored: SeriesDetector = postcard::from_bytes(&bytes).unwrap();
        for _ in 0..50 {
            let x = rng.normal(10.0, 1.0);
            assert_eq!(det.observe(x), restored.observe(x));
        }
    }
}
