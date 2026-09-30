//! Per-series scoring for a root-cause analysis window.
//!
//! The window is split at the anomaly time `t0` into a *reference* period
//! (before) and an *abnormal* period (after). Each series is standardised
//! with robust statistics of the reference period and summarised by a few
//! numbers: how far it went in its harmful direction, how persistently, and
//! when it started moving.

use etio_core::Direction;
use etio_core::stats::robust::{self, RobustSummary};
use serde::{Deserialize, Serialize};

/// How series are standardised and summarised.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScoreConfig {
    /// Relative noise floor (fraction of the series' magnitude).
    pub rel_floor: f64,
    /// Minimum number of finite reference points to score a series.
    pub min_reference: usize,
    /// Harmful score above which a series counts as anomalous.
    pub threshold: f64,
    /// Consecutive points above `threshold` required to call an onset.
    pub onset_persistence: usize,
    /// How far before `t0` to look for onsets, in seconds.
    pub onset_lookback_s: f64,
}

impl Default for ScoreConfig {
    fn default() -> Self {
        Self { rel_floor: 1e-3, min_reference: 10, threshold: 3.0, onset_persistence: 3, onset_lookback_s: 30.0 }
    }
}

/// The scoring convention.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scaling {
    /// Median and the larger of scaled MAD/IQR, with relative floors,
    /// projected on the harmful direction. Etio's default.
    Robust,
    /// BARO's convention (Pham et al., FSE 2024, as released in RCAEval):
    /// median and raw IQR (an IQR below `10ε` is replaced by one), *signed*
    /// maximum (only increases count), and series that are constant in either
    /// period are dropped. Kept bit-for-bit for faithful baselines.
    Iqr,
    /// RCAEval's N-sigma: mean and population standard deviation (zero
    /// replaced by one), signed maximum, constant series dropped.
    NSigma,
}

/// Summary of one series over the analysis window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeriesScore {
    /// Maximum harmful standardised deviation over the abnormal period.
    pub max: f64,
    /// Median harmful deviation over the abnormal period (spike-resistant).
    pub sustained: f64,
    /// Signed shift of the abnormal median, in reference scales.
    pub shift: f64,
    /// Seconds from `t0` to the onset of the deviation (negative if before).
    pub onset_s: Option<f64>,
    /// Reference median.
    pub reference_median: f64,
    /// Scale used to standardise.
    pub scale: f64,
    /// Value at the point of maximum harmful deviation.
    pub peak_value: f64,
    /// Seconds from `t0` to the point of maximum harmful deviation.
    pub peak_offset_s: f64,
}

/// Indices splitting a time grid into reference and abnormal periods.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Split {
    /// First index of the reference period.
    pub reference_start: usize,
    /// First index of the abnormal period (one past the reference period).
    pub abnormal_start: usize,
    /// One past the last index of the abnormal period.
    pub abnormal_end: usize,
}

/// Scores one series.
///
/// `times` are seconds relative to `t0` (so the abnormal period starts at 0).
/// Returns `None` if the series has too little reference data or no finite
/// value in the abnormal period.
#[must_use]
pub fn score_series(
    values: &[f64],
    times: &[f64],
    split: Split,
    direction: Direction,
    scaling: Scaling,
    cfg: &ScoreConfig,
) -> Option<SeriesScore> {
    let reference = &values[split.reference_start..split.abnormal_start];
    let abnormal = &values[split.abnormal_start..split.abnormal_end];
    let summary = RobustSummary::of(reference)?;
    if summary.n < cfg.min_reference || !abnormal.iter().any(|x| x.is_finite()) {
        return None;
    }

    let (center, scale, direction) = match scaling {
        Scaling::Robust => {
            // The scale is learned from the reference period only: anything
            // computed over the abnormal period would shrink the anomaly it
            // is supposed to measure. Sparse series (more than half zeros)
            // have MAD = IQR = 0; their standard deviation still measures
            // their typical fluctuation. An exactly constant reference gives
            // no scale at all: departures from it are scored by persistence.
            if is_constant(reference) {
                return Some(constant_reference_score(
                    reference[0],
                    abnormal,
                    &times[split.abnormal_start..split.abnormal_end],
                    direction,
                    cfg,
                ));
            }
            let mut scale = summary.robust_sigma();
            if scale <= 0.0 {
                scale = summary.std_dev;
            }
            let scale = scale.max(cfg.rel_floor * summary.median.abs());
            (summary.median, scale, direction)
        }
        Scaling::Iqr | Scaling::NSigma => {
            if is_constant(reference) || is_constant(abnormal) {
                return None;
            }
            if scaling == Scaling::Iqr {
                let iqr = summary.iqr;
                (summary.median, if iqr < 10.0 * f64::EPSILON { 1.0 } else { iqr }, Direction::Up)
            } else {
                #[allow(clippy::cast_precision_loss)]
                let n = summary.n as f64;
                let sd = summary.std_dev * ((n - 1.0) / n).sqrt();
                (summary.mean, if sd < 10.0 * f64::EPSILON { 1.0 } else { sd }, Direction::Up)
            }
        }
    };

    // Etio projects deviations on the harmful direction; the published
    // baselines use the signed deviation (only increases rank high).
    let signed = scaling != Scaling::Robust;
    let harm = |x: f64| {
        let z = (x - center) / scale;
        if signed { z } else { direction.harmful(z) }
    };
    let mut max = f64::NEG_INFINITY;
    let mut peak_idx = split.abnormal_start;
    let mut harmful: Vec<f64> = Vec::with_capacity(abnormal.len());
    for (offset, &x) in abnormal.iter().enumerate() {
        if !x.is_finite() {
            continue;
        }
        let h = harm(x);
        harmful.push(h);
        if h > max {
            max = h;
            peak_idx = split.abnormal_start + offset;
        }
    }
    let sustained = robust::quantile(&harmful, 0.5);
    let shift = (robust::median(abnormal) - center) / scale;

    // Onset: the first point, from a little before t0, that starts a run of
    // `onset_persistence` harmful deviations above the threshold.
    let search_start = {
        let lookback = times[split.reference_start..split.abnormal_start]
            .iter()
            .rposition(|&t| t < -cfg.onset_lookback_s)
            .map_or(split.reference_start, |i| split.reference_start + i + 1);
        lookback.max(split.reference_start)
    };
    let mut onset_s = None;
    let mut run = 0usize;
    let mut run_start = search_start;
    for (i, &x) in values.iter().enumerate().take(split.abnormal_end).skip(search_start) {
        if !x.is_finite() {
            continue;
        }
        if harm(x) > cfg.threshold {
            if run == 0 {
                run_start = i;
            }
            run += 1;
            if run >= cfg.onset_persistence.max(1) {
                onset_s = Some(times[run_start]);
                break;
            }
        } else {
            run = 0;
        }
    }

    Some(SeriesScore {
        max: if max.is_finite() { max } else { 0.0 },
        sustained: if sustained.is_finite() { sustained } else { 0.0 },
        shift: if shift.is_finite() { shift } else { 0.0 },
        onset_s,
        reference_median: summary.median,
        scale,
        peak_value: values[peak_idx],
        peak_offset_s: times[peak_idx],
    })
}

/// Whether a series *went silent*: it stopped reporting when the incident
/// began. A process that crashed or was partitioned away emits nothing, and
/// that absence is evidence.
///
/// The test is a count test, not a fixed threshold, so that it works for
/// series that report every window as well as for sparse ones (a metric
/// exported every 5 s is present in one 1 s window out of five): the series
/// must have been expected to report in at least five abnormal windows given
/// its reference presence rate, and reported in at most a tenth of them.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn went_silent(values: &[f64], split: Split) -> bool {
    let reference = &values[split.reference_start..split.abnormal_start];
    let abnormal = &values[split.abnormal_start..split.abnormal_end];
    if reference.is_empty() || abnormal.is_empty() {
        return false;
    }
    let count = |xs: &[f64]| xs.iter().filter(|x| x.is_finite()).count() as f64;
    let rate = count(reference) / reference.len() as f64;
    let expected = rate * abnormal.len() as f64;
    rate >= 0.1 && expected >= 5.0 && count(abnormal) <= 0.1 * expected
}

/// Scores a series whose reference was exactly constant (an error rate that
/// was always zero, a queue that was always empty). There is no noise scale
/// to measure the departure against, so its strength is its persistence: a
/// departure in the harmful direction scores from `threshold` (one window)
/// to `10 × threshold` (every abnormal window). A service going from zero to
/// constant errors is as strong a signal as a 30-sigma anomaly; a single
/// blip is just past the anomaly threshold.
#[allow(clippy::cast_precision_loss)]
fn constant_reference_score(
    level: f64,
    abnormal: &[f64],
    times: &[f64],
    direction: Direction,
    cfg: &ScoreConfig,
) -> SeriesScore {
    let mut deviating = 0usize;
    let mut finite = 0usize;
    let mut onset_s = None;
    let mut peak = (0.0, level, times.first().copied().unwrap_or(0.0));
    for (x, &t) in abnormal.iter().zip(times) {
        if !x.is_finite() {
            continue;
        }
        finite += 1;
        let d = direction.harmful(x - level);
        if d > 0.0 {
            deviating += 1;
            onset_s.get_or_insert(t);
            if d > peak.0 {
                peak = (d, *x, t);
            }
        }
    }
    let fraction = if finite > 0 { deviating as f64 / finite as f64 } else { 0.0 };
    let max = if deviating > 0 { cfg.threshold * (1.0 + 9.0 * fraction) } else { 0.0 };
    SeriesScore {
        max,
        sustained: if fraction >= 0.5 { max } else { 0.0 },
        shift: if fraction >= 0.5 { max } else { 0.0 },
        onset_s,
        reference_median: level,
        scale: 0.0,
        peak_value: peak.1,
        peak_offset_s: peak.2,
    }
}

/// RCAEval's `drop_constant`: every value equals the first one.
fn is_constant(xs: &[f64]) -> bool {
    xs.first().is_none_or(|&first| xs.iter().all(|&x| x.to_bits() == first.to_bits()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::cast_precision_loss)]
    fn grid(n: usize, t0: usize) -> Vec<f64> {
        (0..n).map(|i| i as f64 - t0 as f64).collect()
    }

    fn split(n: usize, t0: usize) -> Split {
        Split { reference_start: 0, abnormal_start: t0, abnormal_end: n }
    }

    #[test]
    fn step_up_is_scored_with_onset() {
        let mut v: Vec<f64> = (0..200).map(|i| 10.0 + f64::from(i % 3) * 0.1).collect();
        for x in &mut v[120..] {
            *x += 5.0;
        }
        let t = grid(200, 100);
        let s = score_series(&v, &t, split(200, 100), Direction::Up, Scaling::Robust, &ScoreConfig::default()).unwrap();
        assert!(s.max > 20.0, "{s:?}");
        assert!(s.sustained > 20.0);
        assert_eq!(s.onset_s, Some(20.0));
        assert!((s.peak_offset_s - 20.0).abs() < 3.0);
    }

    #[test]
    fn harmless_direction_scores_zero() {
        let mut v: Vec<f64> = (0..200).map(|i| 10.0 + f64::from(i % 3) * 0.1).collect();
        for x in &mut v[100..] {
            *x = 2.0;
        }
        let t = grid(200, 100);
        let s = score_series(&v, &t, split(200, 100), Direction::Up, Scaling::Robust, &ScoreConfig::default()).unwrap();
        assert!(s.max.abs() < 1e-12);
        assert!(s.shift < -10.0, "{}", s.shift);
        assert_eq!(s.onset_s, None);
    }

    #[test]
    fn constant_reference_is_scored_by_persistence() {
        let t = grid(200, 100);
        let cfg = ScoreConfig::default();
        // Zero errors, then errors in half of the abnormal windows.
        let mut v = vec![0.0; 200];
        for x in &mut v[150..] {
            *x = 4.0;
        }
        let s = score_series(&v, &t, split(200, 100), Direction::Up, Scaling::Robust, &cfg).unwrap();
        assert!((s.max - 3.0 * 5.5).abs() < 1e-9, "{}", s.max);
        assert_eq!(s.onset_s, Some(50.0));
        // A single blip is just past the threshold; a harmless drop scores zero.
        let mut blip = vec![0.0; 200];
        blip[120] = 1.0;
        let s = score_series(&blip, &t, split(200, 100), Direction::Up, Scaling::Robust, &cfg).unwrap();
        assert!(s.max > 3.0 && s.max < 3.5, "{}", s.max);
        let mut drop = vec![5.0; 200];
        for x in &mut drop[100..] {
            *x = 1.0;
        }
        let s = score_series(&drop, &t, split(200, 100), Direction::Up, Scaling::Robust, &cfg).unwrap();
        assert!(s.max.abs() < 1e-12);
    }

    #[test]
    fn sparse_reference_falls_back_to_standard_deviation() {
        // Mostly zeros with occasional ones: MAD = IQR = 0 but the series is not constant.
        let mut v: Vec<f64> = (0..200).map(|i| if i % 10 == 0 { 1.0 } else { 0.0 }).collect();
        for x in &mut v[150..] {
            *x = 3.0;
        }
        let t = grid(200, 100);
        let s = score_series(&v, &t, split(200, 100), Direction::Up, Scaling::Robust, &ScoreConfig::default()).unwrap();
        // std of the reference is 0.3, so the jump to 3 scores about 10.
        assert!((s.max - 3.0 / 0.301_511_344_577_763_6).abs() < 1e-6, "{}", s.max);
    }

    #[test]
    fn baro_convention_replaces_zero_iqr_by_one() {
        // Mostly constant reference (IQR = 0) that is not exactly constant.
        let mut v = vec![5.0; 100];
        v[10] = 6.0;
        v[80] = 9.0;
        let t = grid(100, 50);
        let s = score_series(&v, &t, split(100, 50), Direction::Both, Scaling::Iqr, &ScoreConfig::default()).unwrap();
        assert!((s.max - 4.0).abs() < 1e-12);
    }

    #[test]
    fn baro_convention_only_counts_increases_and_drops_constant_series() {
        let t = grid(100, 50);
        let cfg = ScoreConfig::default();
        let mut drop = vec![5.0; 100];
        drop[3] = 5.5;
        for x in &mut drop[50..] {
            *x = 1.0;
        }
        drop[99] = 0.5;
        let s = score_series(&drop, &t, split(100, 50), Direction::Both, Scaling::Iqr, &cfg).unwrap();
        assert!(s.max <= 0.0, "a decrease has a non-positive signed score: {}", s.max);

        let mut flat_then_jump = vec![0.0; 100];
        flat_then_jump[70] = 3.0;
        assert!(score_series(&flat_then_jump, &t, split(100, 50), Direction::Up, Scaling::Iqr, &cfg).is_none());
    }

    #[test]
    fn nsigma_uses_population_standard_deviation() {
        let mut v: Vec<f64> = (0..100).map(|i| if i % 2 == 0 { 1.0 } else { 3.0 }).collect();
        v[60] = 10.0;
        let t = grid(100, 50);
        let s = score_series(&v, &t, split(100, 50), Direction::Up, Scaling::NSigma, &ScoreConfig::default()).unwrap();
        // mean 2, population sd 1 -> (10 - 2) / 1
        assert!((s.max - 8.0).abs() < 1e-9, "{}", s.max);
    }

    #[test]
    fn detects_series_that_went_silent() {
        let mut v = vec![1.0; 100];
        for x in &mut v[52..] {
            *x = f64::NAN;
        }
        assert!(went_silent(&v, split(100, 50)));
        let intermittent: Vec<f64> = (0..100).map(|i| if i % 2 == 0 { 1.0 } else { f64::NAN }).collect();
        assert!(!went_silent(&intermittent, split(100, 50)));
        // A metric exported every fifth window that stops is silent too.
        let mut sparse: Vec<f64> = (0..100).map(|i| if i % 5 == 0 { 1.0 } else { f64::NAN }).collect();
        for x in &mut sparse[50..] {
            *x = f64::NAN;
        }
        assert!(went_silent(&sparse, split(100, 50)));
        // Too short an abnormal period to tell.
        assert!(!went_silent(&sparse[..54], split(54, 50)));
    }

    #[test]
    fn too_little_reference_is_unscorable() {
        let v = vec![1.0; 20];
        let t = grid(20, 5);
        assert!(score_series(&v, &t, split(20, 5), Direction::Up, Scaling::Robust, &ScoreConfig::default()).is_none());
    }
}
