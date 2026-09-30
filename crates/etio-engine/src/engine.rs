//! The streaming engine.
//!
//! [`Engine`] is a synchronous, deterministic state machine. Producers call
//! the `ingest_*` methods with decoded telemetry; a driver calls
//! [`Engine::advance`] with the current time (the wall clock in production,
//! a simulated clock in tests and replays). Everything that depends on time
//! (trace completion, window closing, incident timers) is a function of the
//! values passed to `advance`, never of an internal clock, so feeding the
//! same inputs in the same order always yields the same outputs. That is
//! what makes replaying benchmarks through the production code path, and
//! deterministic simulation testing, possible.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use etio_analysis::SeriesState;
use etio_analysis::detect::Observation;
use etio_analysis::graph::ServiceGraph;
use etio_analysis::rca::{self, RcaError, RcaInput, RcaResult, SeriesInput};
use etio_core::time::duration_nanos;
use etio_core::{Interner, Resolution, SignalCategory, Sym, Timestamp};
use etio_pipeline::logs::{Drain, Severity};
use etio_pipeline::{Span, trace};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

use crate::assembler::{AssemblerStats, TraceAssembler};
use crate::config::{ConfigError, EngineConfig};
use crate::incident::{Action, Incident, IncidentManager, WindowAnomalies};
use crate::records::{LogEntry, MetricKind, MetricPoint};
use crate::store::{Fill, SeriesId, SeriesStore};
use crate::window::{Aggregator, AggregatorStats, MetricAgg, WindowSummary};

/// Something the outside world may want to know about.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A new incident.
    IncidentOpened {
        /// The incident.
        incident: Box<Incident>,
    },
    /// An incident has a new root-cause analysis.
    IncidentAnalyzed {
        /// The incident, including the analysis.
        incident: Box<Incident>,
    },
    /// An incident resolved.
    IncidentResolved {
        /// The incident, including its final analysis.
        incident: Box<Incident>,
    },
}

/// Engine counters.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineStats {
    /// Spans ingested.
    pub spans: u64,
    /// Traces analysed.
    pub traces: u64,
    /// Spans shifted to correct clock skew.
    pub skew_adjusted: u64,
    /// Log records ingested.
    pub logs: u64,
    /// Metric points ingested.
    pub metric_points: u64,
    /// Cumulative points that could not be turned into deltas (first point, stream budget).
    pub metric_points_unpaired: u64,
    /// Windows closed.
    pub windows: u64,
    /// Root-cause analyses run.
    pub analyses: u64,
    /// Analyses that could not run (not enough history).
    pub analyses_skipped: u64,
    /// Aggregator counters.
    pub aggregator: AggregatorStats,
    /// Assembler counters.
    pub assembler: AssemblerStats,
}

#[derive(Clone, Debug)]
struct EdgeInfo {
    calls: u64,
    last_window: i64,
}

#[derive(Clone, Debug)]
struct CounterState {
    start: i64,
    value: f64,
    ts: i64,
}

/// The engine.
pub struct Engine {
    cfg: EngineConfig,
    res: Resolution,
    interner: Arc<Interner>,
    assembler: TraceAssembler,
    aggregator: Aggregator,
    counters: HashMap<u64, CounterState>,
    drains: HashMap<Sym, Drain>,
    store: SeriesStore,
    incidents: IncidentManager,
    edges: HashMap<(String, String), EdgeInfo>,
    entry_seen: HashMap<String, i64>,
    now: i64,
    started: Option<i64>,
    /// Newest telemetry timestamp seen (not persisted).
    latest: i64,
    /// After a restore, windows up to this one are recorded as gaps.
    recovery_until: Option<i64>,
    stats: EngineStats,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine").field("now", &self.now).field("series", &self.store.len()).finish_non_exhaustive()
    }
}

/// How long dependency edges and entry points stay in the graph after they
/// were last observed.
const GRAPH_TTL_WINDOWS: i64 = 360;

/// Version of the snapshot format.
pub const SNAPSHOT_FORMAT: u32 = 2;

/// The durable state of an engine: everything that takes time to learn
/// (series history, detector baselines and tail models, incidents, the
/// dependency graph). Buffered traces and open windows are not included:
/// after a restart, data for windows older than the snapshot is late.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngineSnapshot {
    /// [`SNAPSHOT_FORMAT`] at the time of writing.
    pub format: u32,
    /// Window width the history was recorded with, ns.
    pub resolution: i64,
    /// Series history and detectors.
    pub store: SeriesStore,
    /// Incidents.
    pub incidents: IncidentManager,
    /// Dependency edges: `(caller, callee, calls, last window)`.
    pub edges: Vec<(String, String, u64, i64)>,
    /// Entry points and the last window they were seen.
    pub entry_seen: Vec<(String, i64)>,
    /// Engine clock.
    pub now: i64,
    /// First timestamp ever seen.
    pub started: Option<i64>,
    /// Counters.
    pub stats: EngineStats,
}

/// A snapshot that cannot be restored with the current configuration.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RestoreError {
    /// Written by an incompatible version.
    #[error("snapshot format {found} is not supported (expected {expected})")]
    Format {
        /// Format found.
        found: u32,
        /// Format supported.
        expected: u32,
    },
    /// The window width changed; history cannot be reinterpreted.
    #[error("snapshot was recorded with a {found} ns resolution, the configuration uses {expected} ns")]
    Resolution {
        /// Resolution in the snapshot.
        found: i64,
        /// Configured resolution.
        expected: i64,
    },
    /// The configuration itself is invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
}

impl Engine {
    /// Creates an engine.
    ///
    /// # Errors
    /// Returns a [`ConfigError`] if the configuration is inconsistent.
    pub fn new(cfg: EngineConfig) -> Result<Self, ConfigError> {
        cfg.validate()?;
        let res = cfg.resolution();
        let retention = cfg.windows(cfg.retention);
        Ok(Self {
            assembler: TraceAssembler::new(
                duration_nanos(cfg.trace_timeout),
                cfg.limits.max_buffered_spans,
                cfg.limits.max_spans_per_trace,
            ),
            aggregator: Aggregator::new(res, cfg.limits.max_open_windows),
            counters: HashMap::new(),
            drains: HashMap::new(),
            store: SeriesStore::new(retention, cfg.limits.max_series, cfg.detector.clone()),
            incidents: IncidentManager::new(cfg.incident.clone(), 500),
            interner: Arc::new(Interner::with_capacity(cfg.limits.max_symbols)),
            edges: HashMap::new(),
            entry_seen: HashMap::new(),
            now: i64::MIN,
            started: None,
            latest: i64::MIN,
            recovery_until: None,
            stats: EngineStats::default(),
            res,
            cfg,
        })
    }

    /// Captures the durable state (see [`EngineSnapshot`]).
    #[must_use]
    pub fn snapshot(&self) -> EngineSnapshot {
        let mut edges: Vec<(String, String, u64, i64)> =
            self.edges.iter().map(|((a, b), e)| (a.clone(), b.clone(), e.calls, e.last_window)).collect();
        edges.sort();
        let mut entry_seen: Vec<(String, i64)> = self.entry_seen.iter().map(|(k, v)| (k.clone(), *v)).collect();
        entry_seen.sort();
        EngineSnapshot {
            format: SNAPSHOT_FORMAT,
            resolution: self.res.as_nanos(),
            store: self.store.clone(),
            incidents: self.incidents.clone(),
            edges,
            entry_seen,
            now: self.now,
            started: self.started,
            stats: self.stats(),
        }
    }

    /// Recreates an engine from a snapshot, with a (possibly updated)
    /// configuration. The window width must not have changed.
    ///
    /// # Errors
    /// Returns a [`RestoreError`] if the snapshot is incompatible.
    pub fn restore(cfg: EngineConfig, snap: EngineSnapshot) -> Result<Self, RestoreError> {
        if snap.format != SNAPSHOT_FORMAT {
            return Err(RestoreError::Format { found: snap.format, expected: SNAPSHOT_FORMAT });
        }
        let mut engine = Self::new(cfg)?;
        if snap.resolution != engine.res.as_nanos() {
            return Err(RestoreError::Resolution { found: snap.resolution, expected: engine.res.as_nanos() });
        }
        if let Some(last) = snap.store.last_window() {
            engine.aggregator.start_at(last + 1);
            // Data that was in flight when the snapshot was taken is lost: the
            // next windows are incomplete and must not be judged.
            let in_flight = duration_nanos(engine.cfg.lateness + engine.cfg.trace_timeout);
            let windows = in_flight.div_euclid(engine.res.as_nanos()) + 2;
            engine.recovery_until = Some(last + windows);
        }
        engine.store = snap.store;
        engine.store.set_detector_config(engine.cfg.detector.clone());
        engine.incidents = snap.incidents;
        engine.incidents.set_config(engine.cfg.incident.clone());
        engine.edges = snap
            .edges
            .into_iter()
            .map(|(a, b, calls, last_window)| ((a, b), EdgeInfo { calls, last_window }))
            .collect();
        engine.entry_seen = snap.entry_seen.into_iter().collect();
        engine.now = snap.now;
        engine.started = snap.started;
        engine.stats =
            EngineStats { aggregator: AggregatorStats::default(), assembler: AssemblerStats::default(), ..snap.stats };
        Ok(engine)
    }

    /// The configuration.
    #[must_use]
    pub const fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    /// The interner decoders must use to build records for this engine.
    #[must_use]
    pub fn interner(&self) -> &Arc<Interner> {
        &self.interner
    }

    /// The latest time passed to [`Engine::advance`].
    #[must_use]
    pub const fn now(&self) -> i64 {
        self.now
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> EngineStats {
        EngineStats { aggregator: self.aggregator.stats(), assembler: self.assembler.stats(), ..self.stats }
    }

    /// Spans waiting for their trace to complete.
    #[must_use]
    pub const fn buffered_spans(&self) -> usize {
        self.assembler.buffered_spans()
    }

    /// The hot store (read-only).
    #[must_use]
    pub const fn store(&self) -> &SeriesStore {
        &self.store
    }

    /// Incidents, most recent first.
    pub fn incidents(&self) -> impl Iterator<Item = &Incident> {
        self.incidents.all()
    }

    /// One incident.
    #[must_use]
    pub fn incident(&self, id: &str) -> Option<&Incident> {
        self.incidents.get(id)
    }

    /// The newest telemetry timestamp seen, for clocks driven by event time
    /// (replays, faster-than-real-time simulations).
    #[must_use]
    pub const fn latest_event(&self) -> Option<i64> {
        if self.latest == i64::MIN { None } else { Some(self.latest) }
    }

    fn mark_started(&mut self, ts: i64) {
        self.latest = self.latest.max(ts);
        if self.started.is_none() {
            self.started = Some(ts);
        }
        if self.now == i64::MIN {
            self.now = ts;
        }
    }

    // -- ingestion ---------------------------------------------------------------------

    /// Ingests spans (any order, any batching).
    pub fn ingest_spans(&mut self, spans: impl IntoIterator<Item = Span>) {
        for span in spans {
            self.mark_started(span.end);
            self.stats.spans += 1;
            let now = self.now;
            for released in self.assembler.add(span, now) {
                self.analyze_trace(&released);
            }
        }
    }

    fn analyze_trace(&mut self, spans: &[Span]) {
        let a = trace::analyze(spans);
        self.stats.traces += 1;
        self.stats.skew_adjusted += u64::from(a.skew_adjusted);
        for v in &a.visits {
            self.aggregator.add_visit(v);
        }
        for e in &a.edges {
            self.aggregator.add_edge(e);
        }
        for o in &a.error_origins {
            self.aggregator.add_origin(o);
        }
    }

    /// Ingests metric points.
    pub fn ingest_metrics(&mut self, points: &[MetricPoint]) {
        for p in points {
            self.mark_started(p.ts);
            self.stats.metric_points += 1;
            match p.kind {
                MetricKind::Gauge => {
                    self.aggregator.add_metric(p.service, p.name, p.stream, p.ts, p.value, MetricAgg::Level);
                }
                MetricKind::Delta => {
                    self.aggregator.add_metric(p.service, p.name, p.stream, p.ts, p.value, MetricAgg::Rate);
                }
                // A cumulative counter becomes a per-second rate over the
                // interval since its previous point (see `MetricAgg::StreamRate`).
                // Summing increments per window instead would make the value
                // depend on how exports align with windows: with a 5 s export
                // interval and 5 s windows, jitter leaves some windows empty
                // (a false drop to zero) and others with two increments.
                MetricKind::Cumulative { start } => match self.counter_delta(p.stream, start, p.ts, p.value) {
                    Some((d, dt)) => {
                        #[allow(clippy::cast_precision_loss)]
                        let rate = d / (dt as f64 / 1e9);
                        self.aggregator.add_metric(p.service, p.name, p.stream, p.ts, rate, MetricAgg::StreamRate);
                    }
                    None => self.stats.metric_points_unpaired += 1,
                },
            }
        }
    }

    /// Turns a cumulative value into an increment since the previous point of
    /// the same stream, with the time elapsed (ns). Resets (new start time, or
    /// a decreasing value) restart the stream; after a reset the increment is
    /// the new value, accumulated since the stream's start time when known.
    /// The first point of a stream has no increment.
    fn counter_delta(&mut self, stream: u64, start: i64, ts: i64, value: f64) -> Option<(f64, i64)> {
        if !value.is_finite() {
            return None;
        }
        if !self.counters.contains_key(&stream) && self.counters.len() >= self.cfg.limits.max_streams {
            // Forget streams that have been silent for the whole retention.
            let horizon = ts.saturating_sub(duration_nanos(self.cfg.retention));
            self.counters.retain(|_, c| c.ts >= horizon);
            if self.counters.len() >= self.cfg.limits.max_streams {
                return None;
            }
        }
        match self.counters.get_mut(&stream) {
            Some(c) if ts > c.ts => {
                let reset = (start != 0 && start != c.start) || value < c.value;
                let (delta, since) = if reset {
                    (value, if start > c.ts && start < ts { start } else { c.ts })
                } else {
                    (value - c.value, c.ts)
                };
                *c = CounterState { start, value, ts };
                Some((delta, ts - since))
            }
            Some(_) => None, // duplicate or out-of-order point
            None => {
                self.counters.insert(stream, CounterState { start, value, ts });
                None
            }
        }
    }

    /// Ingests log records.
    pub fn ingest_logs(&mut self, logs: &[LogEntry<'_>]) {
        let horizon = duration_nanos(self.cfg.novelty_horizon);
        let warmup = duration_nanos(self.cfg.novelty_warmup);
        for l in logs {
            self.mark_started(l.ts);
            self.stats.logs += 1;
            let drain_cfg = self.cfg.drain;
            let drain = self.drains.entry(l.service).or_insert_with(|| Drain::new(drain_cfg));
            let m = drain.add(l.body, l.ts);
            let first_seen = drain.cluster(m.cluster).map_or(l.ts, |c| c.first_seen);
            let novel_after = self.started.unwrap_or(l.ts).saturating_add(warmup).max(l.ts.saturating_sub(horizon));
            let novel = first_seen >= novel_after;
            let severity = l.severity.unwrap_or_else(|| Severity::classify(l.body));
            self.aggregator.add_log(l.service, l.ts, severity == Severity::Error, novel);
        }
    }

    // -- time ---------------------------------------------------------------------------

    /// Advances the engine clock to `now` (ns since the epoch): completes idle
    /// traces, closes every window that has waited out its lateness, updates
    /// detectors and incidents, and runs due analyses.
    pub fn advance(&mut self, now: i64) -> Vec<Event> {
        self.mark_started(now);
        self.now = self.now.max(now);
        for released in self.assembler.release_idle(self.now) {
            self.analyze_trace(&released);
        }
        let watermark = self.now.saturating_sub(duration_nanos(self.cfg.lateness));
        let until = self.res.window_of(Timestamp(watermark)).0;
        let summaries = self.aggregator.seal_before(until, &self.interner);
        let mut events = Vec::new();
        for s in summaries {
            events.extend(self.apply_summary(&s));
        }
        events
    }

    /// Edge mode: advances the clock like [`Engine::advance`], but returns the
    /// sealed window summaries instead of applying them, so that they can be
    /// shipped to core nodes. Detection, incidents and analysis do not run.
    pub fn advance_edge(&mut self, now: i64) -> Vec<WindowSummary> {
        self.mark_started(now);
        self.now = self.now.max(now);
        for released in self.assembler.release_idle(self.now) {
            self.analyze_trace(&released);
        }
        let watermark = self.now.saturating_sub(duration_nanos(self.cfg.lateness));
        let until = self.res.window_of(Timestamp(watermark)).0;
        // Heartbeats: an edge without traffic still seals (empty) windows, so
        // that it never holds the cores' windows back.
        self.aggregator.start_at(until);
        self.aggregator.seal_before(until, &self.interner)
    }

    /// Completes every buffered trace and closes every window up to the
    /// latest data (end of a replay, shutdown).
    pub fn flush(&mut self) -> Vec<Event> {
        for released in self.assembler.release_all() {
            self.analyze_trace(&released);
        }
        let horizon = self.now.saturating_add(duration_nanos(self.cfg.lateness) + self.res.as_nanos());
        self.advance(horizon)
    }

    // -- windows ------------------------------------------------------------------------

    /// Applies one complete window summary: records every series, runs the
    /// detectors and the incident policy. Summaries must arrive in window
    /// order. This is also the entry point of the core in distributed mode,
    /// where summaries come from edge nodes instead of the local aggregator.
    pub fn apply_summary(&mut self, s: &WindowSummary) -> Vec<Event> {
        if self.store.last_window().is_some_and(|last| s.window <= last) {
            return Vec::new();
        }
        self.stats.windows += 1;
        let w = s.window;
        // A core in distributed mode receives no raw telemetry: its clock
        // and warm-up start from the summaries themselves.
        self.mark_started(self.res.start_of(etio_core::time::WindowIdx(w)).as_nanos());
        self.now = self.now.max(self.res.end_of(etio_core::time::WindowIdx(w)).as_nanos());
        if self.recovery_until.is_some_and(|until| w <= until) {
            self.store.write_gap(w);
            return Vec::new();
        }
        let res_s = self.res.as_secs_f64();

        // Register series first so that every value lands in one pass.
        let mut values: Vec<(SeriesId, f64)> = Vec::new();
        let mut put = |store: &mut SeriesStore, svc: &str, name: &str, cat: SignalCategory, fill: Fill, v: f64| {
            if let Some(id) = store.get_or_create(svc, name, cat, fill, w) {
                values.push((id, v));
            }
        };
        #[allow(clippy::cast_precision_loss)]
        for (svc, st) in &s.services {
            let n = st.requests as f64;
            put(&mut self.store, svc, "trace_requests", SignalCategory::Traffic, Fill::Zero, n / res_s);
            let q = |sk: &etio_core::sketch::DDSketch, p: f64| {
                if st.requests > 0 { sk.quantile(p) / 1e6 } else { f64::NAN }
            };
            put(
                &mut self.store,
                svc,
                "trace_error_rate",
                SignalCategory::Errors,
                Fill::Missing,
                if st.requests > 0 { st.errors as f64 / n } else { f64::NAN },
            );
            put(
                &mut self.store,
                svc,
                "trace_latency_p50",
                SignalCategory::Latency,
                Fill::Missing,
                q(&st.duration, 0.5),
            );
            put(
                &mut self.store,
                svc,
                "trace_latency_p95",
                SignalCategory::Latency,
                Fill::Missing,
                q(&st.duration, 0.95),
            );
            put(&mut self.store, svc, "trace_local_p95", SignalCategory::SelfTime, Fill::Missing, q(&st.local, 0.95));
            put(
                &mut self.store,
                svc,
                "trace_error_origins",
                SignalCategory::ErrorOrigin,
                Fill::Zero,
                st.error_origins as f64,
            );
            if st.roots > 0 {
                self.entry_seen.insert(svc.clone(), w);
            }
        }
        #[allow(clippy::cast_precision_loss)]
        for (svc, l) in &s.logs {
            put(&mut self.store, svc, "log_lines", SignalCategory::Traffic, Fill::Zero, l.lines as f64 / res_s);
            put(&mut self.store, svc, "log_errors", SignalCategory::Logs, Fill::Zero, l.errors as f64 / res_s);
            put(&mut self.store, svc, "log_novel_lines", SignalCategory::Logs, Fill::Zero, l.novel as f64 / res_s);
        }
        for ((svc, name), m) in &s.metrics {
            let fill = match m.agg {
                MetricAgg::Level | MetricAgg::StreamRate => Fill::Missing,
                MetricAgg::Rate => Fill::Zero,
            };
            put(&mut self.store, svc, name, SignalCategory::classify_metric_name(name), fill, m.value(self.res));
        }
        // Calls into each service as its clients saw them. These signals exist
        // even when the callee emits nothing (it is down, or it is a datastore
        // without tracing) and they carry network delay and loss, which the
        // callee's own spans cannot see.
        let mut inbound: BTreeMap<&str, (u64, u64, etio_core::sketch::DDSketch)> = BTreeMap::new();
        for ((a, b), e) in &s.edges {
            let info = self.edges.entry((a.clone(), b.clone())).or_insert(EdgeInfo { calls: 0, last_window: w });
            info.calls += e.calls;
            info.last_window = w;
            let acc = inbound.entry(b.as_str()).or_insert_with(|| (0, 0, e.duration.clone()));
            if acc.0 > 0 {
                let _ = acc.2.merge(&e.duration);
            }
            acc.0 += e.calls;
            acc.1 += e.errors;
        }
        #[allow(clippy::cast_precision_loss)]
        for (callee, (calls, errors, sketch)) in inbound {
            let n = calls as f64;
            put(&mut self.store, callee, "inbound_requests", SignalCategory::Traffic, Fill::Zero, n / res_s);
            put(
                &mut self.store,
                callee,
                "inbound_error_rate",
                SignalCategory::Errors,
                Fill::Missing,
                if calls > 0 { errors as f64 / n } else { f64::NAN },
            );
            put(
                &mut self.store,
                callee,
                "inbound_latency_p95",
                SignalCategory::Latency,
                Fill::Missing,
                if calls > 0 { sketch.quantile(0.95) / 1e6 } else { f64::NAN },
            );
        }

        let mut row = vec![f64::NAN; self.store.len()];
        for (id, v) in values {
            row[id as usize] = v;
        }
        let observations = self.store.write_window(w, &row);
        if self.warming_up(w) {
            return Vec::new();
        }
        let anomalies = self.window_anomalies(w, &observations);
        let actions = self.incidents.on_window(&anomalies);
        self.run_actions(actions)
    }

    /// Whether window `w` falls in the warm-up period after the first data.
    fn warming_up(&self, w: i64) -> bool {
        let Some(started) = self.started else { return true };
        let warmup = self.cfg.incident.warmup.map_or_else(
            || {
                let d = &self.cfg.detector;
                i64::try_from(d.min_baseline + d.spot.calibration).unwrap_or(i64::MAX)
            },
            |dur| i64::try_from(self.cfg.windows(dur)).unwrap_or(i64::MAX),
        );
        w < self.res.window_of(Timestamp(started)).0.saturating_add(warmup)
    }

    fn entry_points(&self, w: i64) -> BTreeSet<String> {
        if !self.cfg.incident.entry_points.is_empty() {
            return self.cfg.incident.entry_points.iter().cloned().collect();
        }
        self.entry_seen.iter().filter(|(_, last)| w - **last <= GRAPH_TTL_WINDOWS).map(|(s, _)| s.clone()).collect()
    }

    fn window_anomalies(&self, w: i64, observations: &[Observation]) -> WindowAnomalies {
        let entry_points = self.entry_points(w);
        let mut services: BTreeMap<String, (i64, f64)> = BTreeMap::new();
        let mut signals: BTreeMap<String, String> = BTreeMap::new();
        let mut user_facing: BTreeSet<String> = BTreeSet::new();
        let min_surprise = self.cfg.incident.min_surprise;
        let confirm = self.cfg.incident.confirm_windows.max(1);
        for (i, o) in observations.iter().enumerate() {
            // `episode_age` counts the windows since the anomaly began,
            // including the lead a CUSUM detection attributes to the shift.
            if o.state != SeriesState::Anomalous || o.episode_age + 1 < confirm || o.surprise < min_surprise {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let meta = self.store.meta(i as SeriesId);
            if self.cfg.incident.exclude.contains(&meta.service) || etio_core::signal::is_host_scoped(&meta.name) {
                continue;
            }
            // Traffic changes are symptoms, except at the entry points where
            // they measure what users experience.
            if meta.category == SignalCategory::Traffic && !entry_points.contains(&meta.service) {
                continue;
            }
            if entry_points.contains(&meta.service)
                && matches!(meta.category, SignalCategory::Latency | SignalCategory::Errors | SignalCategory::Traffic)
            {
                user_facing.insert(meta.service.clone());
            }
            let onset = self.res.start_of(etio_core::time::WindowIdx(w - i64::from(o.episode_age))).as_nanos();
            let e = services.entry(meta.service.clone()).or_insert((onset, f64::NEG_INFINITY));
            e.0 = e.0.min(onset);
            if o.surprise > e.1 {
                e.1 = o.surprise;
                signals.insert(meta.service.clone(), meta.name.clone());
            }
        }
        WindowAnomalies {
            window: w,
            end: self.res.end_of(etio_core::time::WindowIdx(w)).as_nanos(),
            services,
            signals,
            user_facing,
        }
    }

    fn run_actions(&mut self, actions: Vec<Action>) -> Vec<Event> {
        let mut events = Vec::new();
        for action in actions {
            match action {
                Action::Opened(id) => {
                    if let Some(i) = self.incidents.get(&id) {
                        events.push(Event::IncidentOpened { incident: Box::new(i.clone()) });
                    }
                }
                Action::Analyze(id) => {
                    if self.analyze_incident(&id)
                        && let Some(i) = self.incidents.get(&id)
                    {
                        events.push(Event::IncidentAnalyzed { incident: Box::new(i.clone()) });
                    }
                }
                Action::Resolved(id) => {
                    self.analyze_incident(&id);
                    if let Some(i) = self.incidents.get(&id) {
                        events.push(Event::IncidentResolved { incident: Box::new(i.clone()) });
                    }
                }
            }
        }
        events
    }

    fn analyze_incident(&mut self, id: &str) -> bool {
        let Some((start, last, resolved)) =
            self.incidents.get(id).map(|i| (i.start, i.last_anomaly_at, i.resolved_at.is_some()))
        else {
            return false;
        };
        // A resolved incident is analysed over its anomalous period only: the
        // quiet recovery that ended it is not evidence about its cause.
        let end = resolved.then(|| last.saturating_add(2 * self.res.as_nanos()));
        match self.analyze_between(start, end) {
            Ok(result) => {
                self.stats.analyses += 1;
                self.incidents.record_analysis(id, result);
                true
            }
            Err(_) => {
                self.stats.analyses_skipped += 1;
                false
            }
        }
    }

    // -- analysis -----------------------------------------------------------------------

    /// The current dependency graph (edges seen recently).
    #[must_use]
    pub fn graph(&self) -> ServiceGraph {
        let now_w = self.store.last_window().unwrap_or(0);
        let mut edges: Vec<(&(String, String), &EdgeInfo)> =
            self.edges.iter().filter(|(_, e)| now_w - e.last_window <= GRAPH_TTL_WINDOWS).collect();
        edges.sort_by(|a, b| a.0.cmp(b.0));
        let mut g = ServiceGraph::new();
        for ((a, b), e) in edges {
            #[allow(clippy::cast_precision_loss)]
            g.add_edge(a, b, e.calls as f64);
        }
        g
    }

    /// Builds the analysis input for an anomaly starting at `start` (ns).
    ///
    /// # Errors
    /// Returns an error if there is no data or not enough history.
    pub fn rca_input(&self, start: i64) -> Result<RcaInput, RcaError> {
        self.rca_input_between(start, None)
    }

    /// Builds the analysis input for an anomaly between `start` and `end`
    /// (ns; `None` means up to the latest data).
    ///
    /// # Errors
    /// Returns an error if there is no data or not enough history.
    pub fn rca_input_between(&self, start: i64, end: Option<i64>) -> Result<RcaInput, RcaError> {
        let Some(latest) = self.store.last_window() else { return Err(RcaError::NoSeries) };
        let last = end.map_or(latest, |e| self.res.window_of(Timestamp(e)).0.min(latest));
        let start_w = self.res.window_of(Timestamp(start)).0;
        let reference = i64::try_from(self.cfg.windows(self.cfg.incident.reference)).unwrap_or(i64::MAX);
        let cap = i64::try_from(self.store.capacity()).unwrap_or(i64::MAX);
        let from = (start_w - reference).max(last - cap + 1);
        let to = last;
        if from >= start_w || start_w > to {
            return Err(RcaError::AnomalyTime(Timestamp(start).as_secs_f64()));
        }
        let times: Vec<f64> =
            (from..=to).map(|w| self.res.start_of(etio_core::time::WindowIdx(w)).as_secs_f64()).collect();
        let series = self
            .store
            .all_meta()
            .iter()
            .enumerate()
            .map(|(i, m)| SeriesInput {
                service: m.service.clone(),
                name: m.name.clone(),
                category: m.category,
                direction: Some(m.direction),
                #[allow(clippy::cast_possible_truncation)]
                values: self.store.read(i as SeriesId, from, to),
            })
            .collect();
        let graph = self.graph();
        Ok(RcaInput {
            times,
            anomaly_time: self.res.start_of(etio_core::time::WindowIdx(start_w)).as_secs_f64(),
            series,
            graph: (!graph.is_empty()).then_some(graph),
            exclude: self.cfg.incident.exclude.clone(),
        })
    }

    /// Runs a root-cause analysis for an anomaly starting at `start` (ns),
    /// using the configured method and model.
    ///
    /// # Errors
    /// Returns an error if there is no data or not enough history.
    pub fn analyze_at(&self, start: i64) -> Result<RcaResult, RcaError> {
        self.analyze_between(start, None)
    }

    /// Runs a root-cause analysis over `[start, end]` (ns; `None`: up to now).
    ///
    /// # Errors
    /// Returns an error if there is no data or not enough history.
    pub fn analyze_between(&self, start: i64, end: Option<i64>) -> Result<RcaResult, RcaError> {
        let input = self.rca_input_between(start, end)?;
        rca::analyze(&input, &self.cfg.rca)
    }
}
