//! Offline conversion of recorded traces and logs into aligned series.
//!
//! The streaming engine aggregates telemetry window by window as it arrives.
//! Benchmarks and incident post-mortems start from recorded telemetry
//! instead; this module turns it into the same per-service series, using the
//! same trace analysis ([`crate::trace`]), the same latency sketches and the
//! same template miner, so that offline evaluation measures what the engine
//! would have computed online.

use std::collections::BTreeMap;

use etio_core::sketch::DDSketch;
use etio_core::{Interner, Resolution, SignalCategory, Sym, Timestamp};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

use crate::logs::{Drain, DrainConfig, Severity};
use crate::span::Span;
use crate::trace;

/// Relative accuracy of latency sketches (shared with the streaming aggregator).
pub const SKETCH_ACCURACY: f64 = 0.01;

/// A named, regularly sampled series.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Series {
    /// Owning service.
    pub service: String,
    /// Series name.
    pub name: String,
    /// Semantic class.
    pub category: SignalCategory,
    /// One value per window; NaN where undefined.
    pub values: Vec<f64>,
}

/// Series sharing one time grid, plus the dependency edges observed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeriesTable {
    /// Start of the first window.
    pub start: Timestamp,
    /// Window width.
    pub resolution: Resolution,
    /// Number of windows.
    pub len: usize,
    /// The series, sorted by service then name.
    pub series: Vec<Series>,
    /// `(caller, callee, calls)` edges, sorted.
    pub edges: Vec<(String, String, u64)>,
}

impl SeriesTable {
    /// Start of every window, in seconds since the epoch.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn times_secs(&self) -> Vec<f64> {
        (0..self.len).map(|i| (self.start.as_nanos() + self.resolution.as_nanos() * i as i64) as f64 / 1e9).collect()
    }
}

/// Counters describing a conversion, for data-quality reporting.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchStats {
    /// Traces analysed.
    pub traces: u64,
    /// Spans analysed.
    pub spans: u64,
    /// Spans shifted to correct clock skew.
    pub skew_adjusted: u64,
    /// Spans with a missing parent.
    pub orphans: u64,
    /// Observations outside the requested time range.
    pub out_of_range: u64,
    /// Log templates discovered.
    pub templates: u64,
}

fn grid_len(start: Timestamp, end: Timestamp, res: Resolution) -> usize {
    let span = end.as_nanos().saturating_sub(start.as_nanos()).max(0);
    usize::try_from(span.div_euclid(res.as_nanos()) + 1).unwrap_or(0)
}

fn window(ts: i64, start: Timestamp, res: Resolution, len: usize) -> Option<usize> {
    let offset = ts.checked_sub(start.as_nanos())?;
    if offset < 0 {
        return None;
    }
    usize::try_from(offset / res.as_nanos()).ok().filter(|&w| w < len)
}

#[derive(Clone)]
struct TraceArrays {
    requests: Vec<f64>,
    error_rate: Vec<f64>,
    p50: Vec<f64>,
    p95: Vec<f64>,
    local_p95: Vec<f64>,
    origins: Vec<f64>,
}

impl TraceArrays {
    fn new(len: usize) -> Self {
        Self {
            requests: vec![0.0; len],
            error_rate: vec![f64::NAN; len],
            p50: vec![f64::NAN; len],
            p95: vec![f64::NAN; len],
            local_p95: vec![f64::NAN; len],
            origins: vec![0.0; len],
        }
    }
}

/// Converts spans into per-service trace series on the grid `[start, end]`.
///
/// Spans may arrive in any order; they are grouped by trace. Produces, for
/// every service with at least one entry span:
///
/// | name | category | meaning |
/// |---|---|---|
/// | `trace_requests` | traffic | entries per second |
/// | `trace_error_rate` | errors | failed entries / entries |
/// | `trace_latency_p50`, `trace_latency_p95` | latency | entry-span duration, ms |
/// | `trace_local_p95` | self time | time spent inside the service, ms |
/// | `trace_error_origins` | error origin | errors originating in the service |
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn trace_series(
    spans: &mut [Span],
    interner: &Interner,
    start: Timestamp,
    end: Timestamp,
    resolution: Resolution,
) -> (SeriesTable, BatchStats) {
    let len = grid_len(start, end, resolution);
    let mut stats = BatchStats::default();
    spans.sort_unstable_by_key(|s| s.trace_id);

    // (window, service) -> observations; sorted so each group is contiguous.
    let mut visits: Vec<(u32, Sym, i64, i64, bool)> = Vec::new();
    let mut calls: Vec<(u32, Sym, i64, bool)> = Vec::new();
    let mut origins: Vec<(u32, Sym)> = Vec::new();
    let mut edges: HashMap<(Sym, Sym), u64> = HashMap::new();
    let mut from = 0;
    while from < spans.len() {
        let id = spans[from].trace_id;
        let to = from + spans[from..].partition_point(|s| s.trace_id == id);
        let a = trace::analyze(&spans[from..to]);
        stats.traces += 1;
        stats.spans += (to - from) as u64;
        stats.skew_adjusted += u64::from(a.skew_adjusted);
        stats.orphans += u64::from(a.orphans);
        for v in &a.visits {
            match window(v.end, start, resolution, len) {
                #[allow(clippy::cast_possible_truncation)]
                Some(w) => visits.push((w as u32, v.service, v.duration, v.local, v.error)),
                None => stats.out_of_range += 1,
            }
        }
        for o in &a.error_origins {
            if let Some(w) = window(o.end, start, resolution, len) {
                #[allow(clippy::cast_possible_truncation)]
                origins.push((w as u32, o.service));
            }
        }
        for e in &a.edges {
            *edges.entry((e.caller, e.callee)).or_default() += 1;
            if let Some(w) = window(e.end, start, resolution, len) {
                #[allow(clippy::cast_possible_truncation)]
                calls.push((w as u32, e.callee, e.duration, e.error));
            }
        }
        from = to;
    }

    let mut per_service: BTreeMap<String, TraceArrays> = BTreeMap::new();
    let mut names: HashMap<Sym, String> = HashMap::new();
    let mut name_of = |s: Sym| names.entry(s).or_insert_with(|| interner.resolve(s).to_string()).clone();

    visits.sort_unstable_by_key(|&(w, s, ..)| (w, s));
    let mut duration = DDSketch::new(SKETCH_ACCURACY, 2048).unwrap_or_default();
    let mut local = duration.clone();
    let res_s = resolution.as_secs_f64();
    let mut i = 0;
    while i < visits.len() {
        let (w, svc, ..) = visits[i];
        duration.clear();
        local.clear();
        let mut errors = 0u64;
        let mut j = i;
        while j < visits.len() && visits[j].0 == w && visits[j].1 == svc {
            let (_, _, d, l, e) = visits[j];
            duration.add(d as f64);
            local.add(l as f64);
            errors += u64::from(e);
            j += 1;
        }
        let n = (j - i) as f64;
        let arr = per_service.entry(name_of(svc)).or_insert_with(|| TraceArrays::new(len));
        let w = w as usize;
        arr.requests[w] = n / res_s;
        arr.error_rate[w] = errors as f64 / n;
        arr.p50[w] = duration.quantile(0.5) / 1e6;
        arr.p95[w] = duration.quantile(0.95) / 1e6;
        arr.local_p95[w] = local.quantile(0.95) / 1e6;
        i = j;
    }
    for (w, svc) in origins {
        let arr = per_service.entry(name_of(svc)).or_insert_with(|| TraceArrays::new(len));
        arr.origins[w as usize] += 1.0;
    }

    // Calls into each service, as the callers saw them (see the engine).
    let mut inbound: BTreeMap<String, [Vec<f64>; 3]> = BTreeMap::new();
    calls.sort_unstable_by_key(|&(w, s, ..)| (w, s));
    let mut i = 0;
    while i < calls.len() {
        let (w, svc, ..) = calls[i];
        duration.clear();
        let mut errors = 0u64;
        let mut j = i;
        while j < calls.len() && calls[j].0 == w && calls[j].1 == svc {
            duration.add(calls[j].2 as f64);
            errors += u64::from(calls[j].3);
            j += 1;
        }
        let n = (j - i) as f64;
        let arr =
            inbound.entry(name_of(svc)).or_insert_with(|| [vec![0.0; len], vec![f64::NAN; len], vec![f64::NAN; len]]);
        let w = w as usize;
        arr[0][w] = n / res_s;
        arr[1][w] = errors as f64 / n;
        arr[2][w] = duration.quantile(0.95) / 1e6;
        i = j;
    }

    let mut series = Vec::with_capacity(per_service.len() * 6);
    for (service, a) in per_service {
        let mut push = |name: &str, category, values| {
            series.push(Series { service: service.clone(), name: name.to_owned(), category, values });
        };
        push("trace_error_origins", SignalCategory::ErrorOrigin, a.origins);
        push("trace_error_rate", SignalCategory::Errors, a.error_rate);
        push("trace_latency_p50", SignalCategory::Latency, a.p50);
        push("trace_latency_p95", SignalCategory::Latency, a.p95);
        push("trace_local_p95", SignalCategory::SelfTime, a.local_p95);
        push("trace_requests", SignalCategory::Traffic, a.requests);
    }
    for (service, [requests, error_rate, latency]) in inbound {
        series.push(Series {
            service: service.clone(),
            name: "inbound_error_rate".into(),
            category: SignalCategory::Errors,
            values: error_rate,
        });
        series.push(Series {
            service: service.clone(),
            name: "inbound_latency_p95".into(),
            category: SignalCategory::Latency,
            values: latency,
        });
        series.push(Series {
            service,
            name: "inbound_requests".into(),
            category: SignalCategory::Traffic,
            values: requests,
        });
    }
    series.sort_by(|a, b| (&a.service, &a.name).cmp(&(&b.service, &b.name)));
    let mut edge_list: Vec<(String, String, u64)> =
        edges.into_iter().map(|((a, b), n)| (name_of(a), name_of(b), n)).collect();
    edge_list.sort();
    (SeriesTable { start, resolution, len, series, edges: edge_list }, stats)
}

/// A log record for [`log_series`].
#[derive(Copy, Clone, Debug)]
pub struct LogRecord<'a> {
    /// Timestamp, ns since the epoch.
    pub ts: i64,
    /// Emitting service.
    pub service: &'a str,
    /// Message body.
    pub message: &'a str,
    /// Severity, if the record carries one (otherwise it is classified from the text).
    pub severity: Option<Severity>,
}

/// Converts log records into per-service series on the grid `[start, end]`.
///
/// Produces `log_lines` (traffic, lines per second), `log_errors` (logs,
/// error lines per second) and `log_novel_lines` (logs, lines per second
/// whose template first appeared after `novelty_after`). Records are
/// processed in timestamp order, one template miner per service.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn log_series(
    records: &mut [LogRecord<'_>],
    start: Timestamp,
    end: Timestamp,
    resolution: Resolution,
    novelty_after: Timestamp,
    drain: DrainConfig,
) -> (SeriesTable, BatchStats) {
    let len = grid_len(start, end, resolution);
    let mut stats = BatchStats::default();
    records.sort_by_key(|r| r.ts);
    let res_s = resolution.as_secs_f64();

    struct Acc {
        drain: Drain,
        lines: Vec<f64>,
        errors: Vec<f64>,
        novel: Vec<f64>,
    }
    let mut per_service: BTreeMap<&str, Acc> = BTreeMap::new();
    for r in records.iter() {
        let acc = per_service.entry(r.service).or_insert_with(|| Acc {
            drain: Drain::new(drain),
            lines: vec![0.0; len],
            errors: vec![0.0; len],
            novel: vec![0.0; len],
        });
        let m = acc.drain.add(r.message, r.ts);
        if m.created {
            stats.templates += 1;
        }
        let Some(w) = window(r.ts, start, resolution, len) else {
            stats.out_of_range += 1;
            continue;
        };
        acc.lines[w] += 1.0 / res_s;
        let severity = r.severity.unwrap_or_else(|| Severity::classify(r.message));
        if severity == Severity::Error {
            acc.errors[w] += 1.0 / res_s;
        }
        let first = acc.drain.cluster(m.cluster).map_or(r.ts, |c| c.first_seen);
        if first > novelty_after.as_nanos() {
            acc.novel[w] += 1.0 / res_s;
        }
    }
    let mut series = Vec::with_capacity(per_service.len() * 3);
    for (service, a) in per_service {
        series.push(Series {
            service: service.to_owned(),
            name: "log_errors".into(),
            category: SignalCategory::Logs,
            values: a.errors,
        });
        series.push(Series {
            service: service.to_owned(),
            name: "log_lines".into(),
            category: SignalCategory::Traffic,
            values: a.lines,
        });
        series.push(Series {
            service: service.to_owned(),
            name: "log_novel_lines".into(),
            category: SignalCategory::Logs,
            values: a.novel,
        });
    }
    (SeriesTable { start, resolution, len, series, edges: Vec::new() }, stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::{SpanKind, SpanStatus};

    #[allow(clippy::too_many_arguments)]
    fn span(
        interner: &Interner,
        trace: u128,
        id: u64,
        parent: u64,
        svc: &str,
        start_ms: i64,
        dur_ms: i64,
        err: bool,
    ) -> Span {
        Span {
            trace_id: trace,
            span_id: id,
            parent_id: parent,
            service: interner.intern(svc),
            operation: interner.intern("op"),
            kind: SpanKind::Unspecified,
            start: start_ms * 1_000_000,
            end: (start_ms + dur_ms) * 1_000_000,
            status: if err { SpanStatus::Error } else { SpanStatus::Unset },
            peer: Sym::EMPTY,
        }
    }

    fn get<'a>(t: &'a SeriesTable, svc: &str, name: &str) -> &'a [f64] {
        &t.series.iter().find(|s| s.service == svc && s.name == name).unwrap().values
    }

    #[test]
    fn traces_become_per_service_series() {
        let i = Interner::default();
        let mut spans = Vec::new();
        // Two seconds; every trace: frontend 0..90ms -> cart 10..60ms.
        for t in 0..20u64 {
            let base = i64::try_from(t).unwrap() * 100;
            let err = t == 15;
            spans.push(span(&i, u128::from(t), 1, 0, "frontend", base, 90, err));
            spans.push(span(&i, u128::from(t), 2, 1, "cart", base + 10, 50, err));
        }
        let (tab, stats) =
            trace_series(&mut spans, &i, Timestamp::from_secs(0), Timestamp::from_secs(1), Resolution::from_secs(1));
        assert_eq!(stats.traces, 20);
        assert_eq!(tab.len, 2);
        assert_eq!(get(&tab, "frontend", "trace_requests"), &[10.0, 10.0]);
        let p95 = get(&tab, "cart", "trace_latency_p95");
        assert!((p95[0] - 50.0).abs() / 50.0 < 0.011, "{p95:?}");
        let local = get(&tab, "frontend", "trace_local_p95");
        assert!((local[0] - 40.0).abs() / 40.0 < 0.011, "{local:?}");
        assert_eq!(get(&tab, "cart", "trace_error_origins"), &[0.0, 1.0]);
        assert_eq!(get(&tab, "frontend", "trace_error_origins"), &[0.0, 0.0]);
        assert!((get(&tab, "frontend", "trace_error_rate")[1] - 0.1).abs() < 1e-12);
        assert_eq!(tab.edges, vec![("frontend".to_owned(), "cart".to_owned(), 20)]);
        assert_eq!(tab.times_secs(), vec![0.0, 1.0]);
    }

    #[test]
    fn empty_windows_are_nan_for_latency_and_zero_for_counts() {
        let i = Interner::default();
        let mut spans = vec![span(&i, 1, 1, 0, "a", 0, 10, false)];
        let (tab, _) =
            trace_series(&mut spans, &i, Timestamp::from_secs(0), Timestamp::from_secs(2), Resolution::from_secs(1));
        assert!(get(&tab, "a", "trace_latency_p50")[1].is_nan());
        assert!(get(&tab, "a", "trace_requests")[1].abs() < 1e-12);
    }

    #[test]
    fn logs_become_rates_with_novelty() {
        let mut records = vec![
            LogRecord { ts: 0, service: "cart", message: "request 1 served", severity: None },
            LogRecord { ts: 500_000_000, service: "cart", message: "request 2 served", severity: None },
            LogRecord { ts: 1_200_000_000, service: "cart", message: "failed to reach redis: timeout", severity: None },
            LogRecord { ts: 1_300_000_000, service: "cart", message: "failed to reach redis: timeout", severity: None },
        ];
        let (tab, stats) = log_series(
            &mut records,
            Timestamp::from_secs(0),
            Timestamp::from_secs(1),
            Resolution::from_secs(1),
            Timestamp::from_millis(900),
            DrainConfig::default(),
        );
        assert_eq!(stats.templates, 2);
        assert_eq!(get(&tab, "cart", "log_lines"), &[2.0, 2.0]);
        assert_eq!(get(&tab, "cart", "log_errors"), &[0.0, 2.0]);
        assert_eq!(get(&tab, "cart", "log_novel_lines"), &[0.0, 2.0]);
    }
}
