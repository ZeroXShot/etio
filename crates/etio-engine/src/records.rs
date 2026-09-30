//! Telemetry records the engine ingests besides spans.

use etio_core::Sym;
use etio_pipeline::logs::Severity;
use serde::{Deserialize, Serialize};

/// How the value of a metric point accumulates over a window.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum MetricKind {
    /// An instantaneous level (memory in use, queue length). Windows report the mean.
    Gauge,
    /// An increment since the previous point. Windows report the rate per second.
    Delta,
    /// A monotonic total since `start` (OTLP cumulative sums, Prometheus
    /// counters). Converted to deltas with per-stream state; resets are
    /// detected from a changed start time or a decreasing value.
    Cumulative {
        /// Start of the accumulation, ns since the epoch (0 if unknown).
        start: i64,
    },
}

/// One metric data point, attributed to a root-cause candidate.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetricPoint {
    /// Candidate the metric describes (service, or host/pod for infrastructure).
    pub service: Sym,
    /// Series name, including any qualifier (for example `cpu` or `db.pool.used`).
    pub name: Sym,
    /// Identity of the underlying stream (resource and attributes), used to
    /// track cumulative counters. Points of different streams with the same
    /// `(service, name)` are aggregated together.
    pub stream: u64,
    /// Timestamp, ns since the epoch.
    pub ts: i64,
    /// Value.
    pub value: f64,
    /// Accumulation semantics.
    pub kind: MetricKind,
}

/// One log record.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LogEntry<'a> {
    /// Timestamp, ns since the epoch.
    pub ts: i64,
    /// Emitting service.
    pub service: Sym,
    /// Message body.
    pub body: &'a str,
    /// Severity, if known (otherwise classified from the body).
    pub severity: Option<Severity>,
}
