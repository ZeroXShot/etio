//! The hot store: every tracked series, its recent history and its detector.
//!
//! All series share one window clock. When a window closes, *every* series
//! receives exactly one value (observed, or its fill value), so ring slots
//! never hold stale data and a range read is a plain slice walk. History is
//! kept for `capacity` windows, which bounds memory at
//! `series × capacity × 8` bytes plus one detector per series.

use etio_analysis::detect::{DetectorConfig, Observation, SeriesDetector};
use etio_core::{Direction, SignalCategory};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

/// Value recorded for a series in a window without observations.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fill {
    /// Counts and rates: nothing happened, so zero.
    Zero,
    /// Levels and quantiles: undefined, so missing.
    Missing,
}

impl Fill {
    const fn value(self) -> f64 {
        match self {
            Self::Zero => 0.0,
            Self::Missing => f64::NAN,
        }
    }
}

/// Description of a series.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeriesMeta {
    /// Owning service (root-cause candidate).
    pub service: String,
    /// Series name.
    pub name: String,
    /// Semantic class.
    pub category: SignalCategory,
    /// Harmful direction.
    pub direction: Direction,
    /// Fill for empty windows.
    pub fill: Fill,
    /// First window with a value.
    pub created: i64,
}

/// Identifier of a series in the store.
pub type SeriesId = u32;

/// The hot store.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeriesStore {
    capacity: usize,
    max_series: usize,
    detector: DetectorConfig,
    index: HashMap<String, HashMap<String, SeriesId>>,
    meta: Vec<SeriesMeta>,
    rings: Vec<Vec<f64>>,
    detectors: Vec<SeriesDetector>,
    /// Latest window written.
    last_window: Option<i64>,
    rejected: u64,
}

impl SeriesStore {
    /// Creates a store keeping `capacity` windows of at most `max_series` series.
    #[must_use]
    pub fn new(capacity: usize, max_series: usize, detector: DetectorConfig) -> Self {
        Self {
            capacity: capacity.max(2),
            max_series,
            detector,
            index: HashMap::new(),
            meta: Vec::new(),
            rings: Vec::new(),
            detectors: Vec::new(),
            last_window: None,
            rejected: 0,
        }
    }

    /// Detector settings used for series created from now on (existing
    /// detectors keep their state and settings).
    pub fn set_detector_config(&mut self, cfg: DetectorConfig) {
        self.detector = cfg;
    }

    /// Number of series.
    #[must_use]
    pub fn len(&self) -> usize {
        self.meta.len()
    }

    /// Whether the store is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.meta.is_empty()
    }

    /// Windows of history kept.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Series refused because the store was full.
    #[must_use]
    pub const fn rejected(&self) -> u64 {
        self.rejected
    }

    /// Latest window written.
    #[must_use]
    pub const fn last_window(&self) -> Option<i64> {
        self.last_window
    }

    /// Metadata of a series.
    #[must_use]
    pub fn meta(&self, id: SeriesId) -> &SeriesMeta {
        &self.meta[id as usize]
    }

    /// All series metadata, indexed by id.
    #[must_use]
    pub fn all_meta(&self) -> &[SeriesMeta] {
        &self.meta
    }

    /// Looks up a series.
    #[must_use]
    pub fn find(&self, service: &str, name: &str) -> Option<SeriesId> {
        self.index.get(service)?.get(name).copied()
    }

    /// Returns the id of a series, creating it on first use.
    ///
    /// Returns `None` if the store is full (the series is counted as rejected).
    pub fn get_or_create(
        &mut self,
        service: &str,
        name: &str,
        category: SignalCategory,
        fill: Fill,
        window: i64,
    ) -> Option<SeriesId> {
        if let Some(id) = self.find(service, name) {
            return Some(id);
        }
        if self.meta.len() >= self.max_series {
            self.rejected += 1;
            return None;
        }
        let id = SeriesId::try_from(self.meta.len()).ok()?;
        let direction = category.default_direction();
        self.meta.push(SeriesMeta {
            service: service.to_owned(),
            name: name.to_owned(),
            category,
            direction,
            fill,
            created: window,
        });
        self.rings.push(vec![f64::NAN; self.capacity]);
        self.detectors.push(SeriesDetector::new(self.detector.clone(), direction));
        self.index.entry(service.to_owned()).or_default().insert(name.to_owned(), id);
        Some(id)
    }

    fn slot(&self, window: i64) -> usize {
        let cap = i64::try_from(self.capacity).unwrap_or(i64::MAX);
        usize::try_from(window.rem_euclid(cap)).unwrap_or(0)
    }

    /// Writes one window: `values[id]` for every series (NaN means "no
    /// observation", replaced by the series' fill), runs every detector, and
    /// returns the observations. Windows must be written in increasing order.
    ///
    /// # Panics
    /// Panics if `values` does not have one entry per series, or if windows
    /// are written out of order.
    pub fn write_window(&mut self, window: i64, values: &[f64]) -> Vec<Observation> {
        assert_eq!(values.len(), self.meta.len(), "one value per series");
        if let Some(last) = self.last_window {
            assert!(window > last, "windows must be written in order ({window} after {last})");
        }
        let slot = self.slot(window);
        let mut out = Vec::with_capacity(values.len());
        for (i, &v) in values.iter().enumerate() {
            let v = if v.is_nan() { self.meta[i].fill.value() } else { v };
            self.rings[i][slot] = v;
            out.push(self.detectors[i].observe(v));
        }
        self.last_window = Some(window);
        out
    }

    /// Records a window in which data is known to be incomplete (for example
    /// around a restart): every series gets a missing value and detectors are
    /// not updated, so partial data neither alarms nor pollutes baselines.
    ///
    /// # Panics
    /// Panics if windows are written out of order.
    pub fn write_gap(&mut self, window: i64) {
        if let Some(last) = self.last_window {
            assert!(window > last, "windows must be written in order ({window} after {last})");
        }
        let slot = self.slot(window);
        for ring in &mut self.rings {
            ring[slot] = f64::NAN;
        }
        self.last_window = Some(window);
    }

    /// Values of a series for windows `from..=to` (NaN outside the retained
    /// history or before the series existed).
    #[must_use]
    pub fn read(&self, id: SeriesId, from: i64, to: i64) -> Vec<f64> {
        let Some(last) = self.last_window else {
            return vec![f64::NAN; usize::try_from(to - from + 1).unwrap_or(0)];
        };
        let cap = i64::try_from(self.capacity).unwrap_or(i64::MAX);
        let meta = &self.meta[id as usize];
        let ring = &self.rings[id as usize];
        (from..=to)
            .map(|w| if w > last || w <= last - cap || w < meta.created { f64::NAN } else { ring[self.slot(w)] })
            .collect()
    }

    /// Whether a series is currently in an anomalous episode.
    #[must_use]
    pub fn is_anomalous(&self, id: SeriesId) -> bool {
        self.detectors[id as usize].is_anomalous()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(cap: usize) -> SeriesStore {
        SeriesStore::new(cap, 10, DetectorConfig::default())
    }

    #[test]
    fn creates_and_finds_series() {
        let mut s = store(8);
        let a = s.get_or_create("cart", "cpu", SignalCategory::Cpu, Fill::Missing, 0).unwrap();
        let b = s.get_or_create("cart", "cpu", SignalCategory::Cpu, Fill::Missing, 5).unwrap();
        assert_eq!(a, b);
        assert_eq!(s.find("cart", "cpu"), Some(a));
        assert_eq!(s.find("cart", "mem"), None);
        assert_eq!(s.meta(a).direction, Direction::Up);
    }

    #[test]
    fn enforces_the_series_budget() {
        let mut s = SeriesStore::new(4, 1, DetectorConfig::default());
        assert!(s.get_or_create("a", "x", SignalCategory::Other, Fill::Zero, 0).is_some());
        assert!(s.get_or_create("a", "y", SignalCategory::Other, Fill::Zero, 0).is_none());
        assert_eq!(s.rejected(), 1);
    }

    #[test]
    fn ring_keeps_recent_history_with_fills() {
        let mut s = store(4);
        let req = s.get_or_create("cart", "req", SignalCategory::Traffic, Fill::Zero, 10).unwrap();
        let lat = s.get_or_create("cart", "lat", SignalCategory::Latency, Fill::Missing, 10).unwrap();
        for w in 10..16 {
            #[allow(clippy::cast_precision_loss)]
            let v = if w == 12 { f64::NAN } else { w as f64 };
            s.write_window(w, &[v, v]);
        }
        // Only windows 12..=15 are retained.
        let r = s.read(req, 10, 16);
        assert!(r[0].is_nan() && r[1].is_nan(), "{r:?}");
        assert!(r[2].abs() < 1e-12, "zero fill: {r:?}");
        assert_eq!(&r[3..6], &[13.0, 14.0, 15.0]);
        assert!(r[6].is_nan(), "future window");
        assert!(s.read(lat, 12, 12)[0].is_nan(), "missing fill");
    }

    #[test]
    fn later_series_have_no_history_before_creation() {
        let mut s = store(8);
        let a = s.get_or_create("a", "x", SignalCategory::Other, Fill::Zero, 0).unwrap();
        s.write_window(0, &[1.0]);
        let b = s.get_or_create("b", "x", SignalCategory::Other, Fill::Zero, 1).unwrap();
        s.write_window(1, &[2.0, 3.0]);
        assert_eq!(s.read(a, 0, 1), vec![1.0, 2.0]);
        let rb = s.read(b, 0, 1);
        assert!(rb[0].is_nan());
        assert!((rb[1] - 3.0).abs() < 1e-12);
    }

    #[test]
    #[should_panic(expected = "in order")]
    fn out_of_order_windows_panic() {
        let mut s = store(4);
        s.write_window(5, &[]);
        s.write_window(5, &[]);
    }
}
