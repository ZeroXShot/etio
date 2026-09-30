//! Event-time windows and their mergeable summaries.
//!
//! Every aggregation window produces a [`WindowSummary`]: counters and
//! latency sketches per service, per dependency edge, per log stream and per
//! metric. Summaries form a commutative monoid under [`WindowSummary::merge`]
//! (counters add, sketches merge, minima and maxima combine), which is what
//! lets the engine split work:
//!
//! * inside one process, shards that each see part of the traffic produce
//!   partial summaries that the analysis core merges;
//! * across processes, edge nodes ship their partial summaries to one or
//!   more core nodes. Because merging is commutative and associative, the
//!   core's result does not depend on arrival order, and deduplicating
//!   (edge, window) pairs makes delivery idempotent. No consensus is needed.
//!
//! Keys are strings so that summaries are meaningful outside the process
//! that produced them; inside the [`Aggregator`] everything is keyed by
//! interned symbols.

use std::collections::BTreeMap;

use etio_core::sketch::DDSketch;
use etio_core::{Interner, Resolution, Sym};
use etio_pipeline::trace::{EdgeObservation, ErrorOrigin, ServiceVisit};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

/// Relative accuracy of every latency sketch.
pub const SKETCH_ACCURACY: f64 = 0.01;
/// Bucket budget of every latency sketch.
pub const SKETCH_BINS: usize = 1024;

fn sketch() -> DDSketch {
    DDSketch::new(SKETCH_ACCURACY, SKETCH_BINS).unwrap_or_default()
}

/// Request statistics of one service in one window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServiceStats {
    /// Entries into the service.
    pub requests: u64,
    /// Failed entries.
    pub errors: u64,
    /// Entries that were trace roots (user-facing requests).
    pub roots: u64,
    /// Duration of entry spans, ns.
    pub duration: DDSketch,
    /// Time spent inside the service, ns.
    pub local: DDSketch,
    /// Errors originating in the service.
    pub error_origins: u64,
}

impl Default for ServiceStats {
    fn default() -> Self {
        Self { requests: 0, errors: 0, roots: 0, duration: sketch(), local: sketch(), error_origins: 0 }
    }
}

impl ServiceStats {
    fn merge(&mut self, o: &Self) {
        self.requests += o.requests;
        self.errors += o.errors;
        self.roots += o.roots;
        self.error_origins += o.error_origins;
        // Sketches always share the engine-wide accuracy; a mismatch would be
        // a programming error, and dropping the other side's sketch keeps the
        // counters correct.
        let _ = self.duration.merge(&o.duration);
        let _ = self.local.merge(&o.local);
    }
}

/// Call statistics of one dependency edge in one window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeStats {
    /// Calls.
    pub calls: u64,
    /// Failed calls.
    pub errors: u64,
    /// Call duration, ns.
    pub duration: DDSketch,
    /// Whether the callee is a virtual node without telemetry.
    pub virtual_callee: bool,
}

impl Default for EdgeStats {
    fn default() -> Self {
        Self { calls: 0, errors: 0, duration: sketch(), virtual_callee: false }
    }
}

impl EdgeStats {
    fn merge(&mut self, o: &Self) {
        self.calls += o.calls;
        self.errors += o.errors;
        self.virtual_callee |= o.virtual_callee;
        let _ = self.duration.merge(&o.duration);
    }
}

/// Log statistics of one service in one window.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogStats {
    /// Lines.
    pub lines: u64,
    /// Error lines.
    pub errors: u64,
    /// Lines whose template first appeared after the warm-up period.
    pub novel: u64,
}

impl LogStats {
    fn merge(&mut self, o: &Self) {
        self.lines += o.lines;
        self.errors += o.errors;
        self.novel += o.novel;
    }
}

/// Whether a metric window reports a level or a rate.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricAgg {
    /// Mean of the observed values.
    #[default]
    Level,
    /// Sum of increments divided by the window width.
    Rate,
    /// Per-second rates of one or more streams (counters): the mean rate of
    /// the points times the number of distinct streams, i.e. the sum over
    /// streams of their mean rate. Unlike [`MetricAgg::Rate`], the value does
    /// not depend on how many export points happen to fall in the window.
    StreamRate,
}

/// Accumulated metric values of one series in one window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetricStats {
    /// Aggregation semantics.
    pub agg: MetricAgg,
    /// Sum of values (levels) or increments (rates).
    pub sum: f64,
    /// Number of values.
    pub count: u64,
    /// Smallest value.
    pub min: f64,
    /// Largest value.
    pub max: f64,
    /// Distinct streams seen, sorted ([`MetricAgg::StreamRate`] only).
    #[serde(default)]
    pub streams: Vec<u64>,
}

impl MetricStats {
    fn new(agg: MetricAgg) -> Self {
        Self { agg, sum: 0.0, count: 0, min: f64::INFINITY, max: f64::NEG_INFINITY, streams: Vec::new() }
    }

    fn add(&mut self, v: f64, stream: u64) {
        self.sum += v;
        self.count += 1;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        if self.agg == MetricAgg::StreamRate
            && let Err(i) = self.streams.binary_search(&stream)
        {
            self.streams.insert(i, stream);
        }
    }

    fn merge(&mut self, o: &Self) {
        self.sum += o.sum;
        self.count += o.count;
        self.min = self.min.min(o.min);
        self.max = self.max.max(o.max);
        for &s in &o.streams {
            if let Err(i) = self.streams.binary_search(&s) {
                self.streams.insert(i, s);
            }
        }
    }

    /// The window's value: the mean for levels, the per-second rate for rates.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn value(&self, resolution: Resolution) -> f64 {
        match self.agg {
            MetricAgg::Level if self.count > 0 => self.sum / self.count as f64,
            MetricAgg::Level => f64::NAN,
            MetricAgg::Rate => self.sum / resolution.as_secs_f64(),
            MetricAgg::StreamRate if self.count > 0 => self.sum / self.count as f64 * self.streams.len().max(1) as f64,
            MetricAgg::StreamRate => f64::NAN,
        }
    }
}

/// Everything observed during one window, by one or more producers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowSummary {
    /// Window index.
    pub window: i64,
    /// Per-service request statistics.
    pub services: BTreeMap<String, ServiceStats>,
    /// Per-edge call statistics, keyed by `(caller, callee)`.
    pub edges: BTreeMap<(String, String), EdgeStats>,
    /// Per-service log statistics.
    pub logs: BTreeMap<String, LogStats>,
    /// Per-series metric statistics, keyed by `(service, metric)`.
    pub metrics: BTreeMap<(String, String), MetricStats>,
}

impl WindowSummary {
    /// An empty summary for `window`.
    #[must_use]
    pub fn empty(window: i64) -> Self {
        Self { window, ..Self::default() }
    }

    /// Whether nothing was observed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.services.is_empty() && self.edges.is_empty() && self.logs.is_empty() && self.metrics.is_empty()
    }

    /// Merges another summary of the same window into this one.
    ///
    /// # Panics
    /// Panics if the windows differ: merging different windows is a bug.
    pub fn merge(&mut self, other: &Self) {
        assert_eq!(self.window, other.window, "merging summaries of different windows");
        for (k, v) in &other.services {
            self.services.entry(k.clone()).or_default().merge(v);
        }
        for (k, v) in &other.edges {
            self.edges.entry(k.clone()).or_default().merge(v);
        }
        for (k, v) in &other.logs {
            self.logs.entry(k.clone()).or_default().merge(v);
        }
        for (k, v) in &other.metrics {
            self.metrics.entry(k.clone()).or_insert_with(|| MetricStats::new(v.agg)).merge(v);
        }
    }
}

#[derive(Default)]
struct Partial {
    services: HashMap<Sym, ServiceStats>,
    edges: HashMap<(Sym, Sym), EdgeStats>,
    logs: HashMap<Sym, LogStats>,
    metrics: HashMap<(Sym, Sym), MetricStats>,
}

/// Counters of the aggregator.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregatorStats {
    /// Observations dropped because their window was already sealed.
    pub late: u64,
    /// Observations dropped because they were too far in the future.
    pub future: u64,
    /// Windows sealed.
    pub sealed: u64,
}

/// Accumulates observations into open windows and seals them in order.
pub struct Aggregator {
    resolution: Resolution,
    open: BTreeMap<i64, Partial>,
    /// First window that has not been sealed yet.
    next_to_seal: Option<i64>,
    max_open_windows: i64,
    stats: AggregatorStats,
}

impl Aggregator {
    /// Creates an aggregator. Observations more than `max_open_windows`
    /// windows ahead of the oldest open window are rejected as clock errors.
    #[must_use]
    pub fn new(resolution: Resolution, max_open_windows: usize) -> Self {
        Self {
            resolution,
            open: BTreeMap::new(),
            next_to_seal: None,
            max_open_windows: i64::try_from(max_open_windows.max(1)).unwrap_or(i64::MAX),
            stats: AggregatorStats::default(),
        }
    }

    /// Counters.
    #[must_use]
    pub const fn stats(&self) -> AggregatorStats {
        self.stats
    }

    /// The first window that will be sealed next, once time advances.
    #[must_use]
    pub const fn next_to_seal(&self) -> Option<i64> {
        self.next_to_seal
    }

    /// Makes sure windows before `w` are never reopened (used on start-up).
    pub fn start_at(&mut self, w: i64) {
        if self.next_to_seal.is_none() {
            self.next_to_seal = Some(w);
        }
    }

    fn partial(&mut self, ts: i64) -> Option<&mut Partial> {
        let w = self.resolution.window_of(etio_core::Timestamp(ts)).0;
        let floor = *self.next_to_seal.get_or_insert(w);
        if w < floor {
            self.stats.late += 1;
            return None;
        }
        if w - floor > self.max_open_windows {
            self.stats.future += 1;
            return None;
        }
        Some(self.open.entry(w).or_default())
    }

    /// Records an entry into a service.
    pub fn add_visit(&mut self, v: &ServiceVisit) {
        if let Some(p) = self.partial(v.end) {
            let s = p.services.entry(v.service).or_default();
            s.requests += 1;
            s.errors += u64::from(v.error);
            s.roots += u64::from(v.root);
            #[allow(clippy::cast_precision_loss)]
            {
                s.duration.add(v.duration as f64);
                s.local.add(v.local as f64);
            }
        }
    }

    /// Records a call between services.
    pub fn add_edge(&mut self, e: &EdgeObservation) {
        if let Some(p) = self.partial(e.end) {
            let s = p.edges.entry((e.caller, e.callee)).or_default();
            s.calls += 1;
            s.errors += u64::from(e.error);
            s.virtual_callee |= e.virtual_callee;
            #[allow(clippy::cast_precision_loss)]
            s.duration.add(e.duration as f64);
        }
    }

    /// Records an error origin.
    pub fn add_origin(&mut self, o: &ErrorOrigin) {
        if let Some(p) = self.partial(o.end) {
            p.services.entry(o.service).or_default().error_origins += 1;
        }
    }

    /// Records a log line.
    pub fn add_log(&mut self, service: Sym, ts: i64, error: bool, novel: bool) {
        if let Some(p) = self.partial(ts) {
            let s = p.logs.entry(service).or_default();
            s.lines += 1;
            s.errors += u64::from(error);
            s.novel += u64::from(novel);
        }
    }

    /// Records a metric value (a level or an increment).
    pub fn add_metric(&mut self, service: Sym, name: Sym, stream: u64, ts: i64, value: f64, agg: MetricAgg) {
        if !value.is_finite() {
            return;
        }
        if let Some(p) = self.partial(ts) {
            p.metrics.entry((service, name)).or_insert_with(|| MetricStats::new(agg)).add(value, stream);
        }
    }

    /// Seals every window strictly before `until` (including windows in which
    /// nothing happened, so consumers see a gap-free sequence).
    pub fn seal_before(&mut self, until: i64, interner: &Interner) -> Vec<WindowSummary> {
        let Some(mut w) = self.next_to_seal else { return Vec::new() };
        let mut out = Vec::new();
        while w < until {
            let p = self.open.remove(&w).unwrap_or_default();
            out.push(resolve(w, p, interner));
            w += 1;
            self.stats.sealed += 1;
        }
        self.next_to_seal = Some(w);
        out
    }
}

fn resolve(window: i64, p: Partial, interner: &Interner) -> WindowSummary {
    let name = |s: Sym| interner.resolve(s).to_string();
    WindowSummary {
        window,
        services: p.services.into_iter().map(|(k, v)| (name(k), v)).collect(),
        edges: p.edges.into_iter().map(|((a, b), v)| ((name(a), name(b)), v)).collect(),
        logs: p.logs.into_iter().map(|(k, v)| (name(k), v)).collect(),
        metrics: p.metrics.into_iter().map(|((a, b), v)| ((name(a), name(b)), v)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const SEC: i64 = 1_000_000_000;

    fn visit(interner: &Interner, svc: &str, end: i64, dur_ms: i64, err: bool) -> ServiceVisit {
        ServiceVisit {
            service: interner.intern(svc),
            operation: Sym::EMPTY,
            end,
            duration: dur_ms * 1_000_000,
            local: dur_ms * 500_000,
            error: err,
            root: true,
        }
    }

    #[test]
    fn seals_in_order_with_gap_free_windows() {
        let i = Interner::default();
        let mut a = Aggregator::new(Resolution::from_secs(10), 100);
        a.add_visit(&visit(&i, "cart", 5 * SEC, 20, false));
        a.add_visit(&visit(&i, "cart", 35 * SEC, 20, true));
        let sealed = a.seal_before(4, &i);
        assert_eq!(sealed.iter().map(|s| s.window).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
        assert_eq!(sealed[0].services["cart"].requests, 1);
        assert!(sealed[1].is_empty() && sealed[2].is_empty());
        assert_eq!(sealed[3].services["cart"].errors, 1);
        // Data for a sealed window is late.
        a.add_visit(&visit(&i, "cart", 15 * SEC, 20, false));
        assert_eq!(a.stats().late, 1);
        assert_eq!(a.stats().sealed, 4);
    }

    #[test]
    fn far_future_data_is_rejected() {
        let i = Interner::default();
        let mut a = Aggregator::new(Resolution::from_secs(1), 10);
        a.add_visit(&visit(&i, "a", 0, 1, false));
        a.add_visit(&visit(&i, "a", 1_000 * SEC, 1, false));
        assert_eq!(a.stats().future, 1);
    }

    #[test]
    fn metrics_report_levels_and_rates() {
        let i = Interner::default();
        let r = Resolution::from_secs(10);
        let mut a = Aggregator::new(r, 100);
        let (svc, mem, req) = (i.intern("cart"), i.intern("mem"), i.intern("req"));
        a.add_metric(svc, mem, 0, SEC, 100.0, MetricAgg::Level);
        a.add_metric(svc, mem, 0, 2 * SEC, 300.0, MetricAgg::Level);
        a.add_metric(svc, req, 0, SEC, 50.0, MetricAgg::Rate);
        a.add_metric(svc, req, 0, 2 * SEC, 30.0, MetricAgg::Rate);
        let s = &a.seal_before(1, &i)[0];
        assert!((s.metrics[&("cart".into(), "mem".into())].value(r) - 200.0).abs() < 1e-12);
        assert!((s.metrics[&("cart".into(), "req".into())].value(r) - 8.0).abs() < 1e-12);
    }

    fn summary(window: i64, reqs: &[(u8, u64, u64)]) -> WindowSummary {
        let mut s = WindowSummary::empty(window);
        for &(svc, n, dur) in reqs {
            let st = s.services.entry(format!("s{}", svc % 4)).or_default();
            st.requests += n;
            #[allow(clippy::cast_precision_loss)]
            st.duration.add_n(dur as f64 + 1.0, n.max(1));
            s.logs.entry(format!("s{}", svc % 3)).or_default().lines += n;
        }
        s
    }

    proptest! {
        #[test]
        fn merge_is_commutative_and_associative(
            a in prop::collection::vec((any::<u8>(), 0u64..50, 0u64..1_000_000), 0..10),
            b in prop::collection::vec((any::<u8>(), 0u64..50, 0u64..1_000_000), 0..10),
            c in prop::collection::vec((any::<u8>(), 0u64..50, 0u64..1_000_000), 0..10),
        ) {
            let (sa, sb, sc) = (summary(7, &a), summary(7, &b), summary(7, &c));
            let mut ab_c = sa.clone(); ab_c.merge(&sb); ab_c.merge(&sc);
            let mut bc = sb.clone(); bc.merge(&sc);
            let mut a_bc = sa.clone(); a_bc.merge(&bc);
            let mut cba = sc.clone(); cba.merge(&sb); cba.merge(&sa);
            prop_assert_eq!(&ab_c, &a_bc);
            prop_assert_eq!(&ab_c, &cba);
            // The identity element.
            let mut e = WindowSummary::empty(7); e.merge(&sa);
            prop_assert_eq!(&e, &sa);
        }
    }

    #[test]
    #[should_panic(expected = "different windows")]
    fn merging_different_windows_panics() {
        let mut a = WindowSummary::empty(1);
        a.merge(&WindowSummary::empty(2));
    }

    #[test]
    fn summaries_serialize_compactly() {
        let mut s = summary(3, &[(1, 10, 5_000_000), (2, 3, 40_000_000)]);
        s.edges.insert(("s1".into(), "s2".into()), EdgeStats { calls: 3, ..EdgeStats::default() });
        let bytes = postcard::to_stdvec(&s).unwrap();
        let back: WindowSummary = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, s);
        assert!(bytes.len() < 600, "{} bytes", bytes.len());
    }
}
