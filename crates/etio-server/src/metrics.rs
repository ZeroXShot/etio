//! Prometheus metrics describing the server itself.
//!
//! An observability tool must be observable: every queue, rejection, decode
//! and analysis is counted, and the engine's internal counters are exported
//! as gauges refreshed on every tick.

use std::sync::atomic::AtomicU64;

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{Histogram, exponential_buckets};
use prometheus_client::registry::Registry;

/// Labels of an ingest request.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RequestLabels {
    /// `traces`, `metrics` or `logs`.
    pub signal: &'static str,
    /// `grpc` or `http`.
    pub transport: &'static str,
    /// `ok`, `invalid`, `too_large`, `unsupported`, `backpressure`, `unauthenticated`.
    pub outcome: &'static str,
}

/// Labels identifying a signal.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct SignalLabels {
    /// `traces`, `metrics` or `logs`.
    pub signal: &'static str,
}

/// Labels of a webhook delivery.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct DeliveryLabels {
    /// `ok` or `failed`.
    pub outcome: &'static str,
}

/// Labels of a distributed-mode counter.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ClusterLabels {
    /// Edge: `enqueued`, `dropped`, `retained`. Core: `accepted`,
    /// `duplicates`, `gaps`, `stale_epoch`, `late`, `lost`, `deadline_releases`.
    pub event: &'static str,
}

/// All server metrics.
pub struct Metrics {
    registry: Registry,
    /// Ingest requests by signal, transport and outcome.
    pub requests: Family<RequestLabels, Counter>,
    /// Items (spans, points, records) accepted.
    pub items: Family<SignalLabels, Counter>,
    /// Items rejected by the decoders.
    pub items_rejected: Family<SignalLabels, Counter>,
    /// Decode time per request.
    pub decode_seconds: Family<SignalLabels, Histogram>,
    /// Batches waiting for the engine.
    pub queue_depth: Gauge,
    /// Series tracked.
    pub series: Gauge,
    /// Spans buffered for trace assembly.
    pub buffered_spans: Gauge,
    /// Windows closed.
    pub windows: Gauge<u64, AtomicU64>,
    /// Observations dropped because their window had closed.
    pub late: Gauge<u64, AtomicU64>,
    /// Traces analysed.
    pub traces: Gauge<u64, AtomicU64>,
    /// Open incidents.
    pub incidents_open: Gauge,
    /// Root-cause analyses run.
    pub analyses: Gauge<u64, AtomicU64>,
    /// Time taken by one engine tick (window closing, detection, analyses).
    pub tick_seconds: Histogram,
    /// Snapshot duration.
    pub snapshot_seconds: Histogram,
    /// Size of the last snapshot.
    pub snapshot_bytes: Gauge,
    /// Webhook deliveries.
    pub deliveries: Family<DeliveryLabels, Counter>,
    /// Window summaries shipped (edge) or received (core).
    pub cluster: Family<ClusterLabels, Gauge<u64, AtomicU64>>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    /// Creates and registers every metric.
    #[must_use]
    pub fn new() -> Self {
        let mut registry = Registry::with_prefix("etio");
        let requests = Family::<RequestLabels, Counter>::default();
        let items = Family::<SignalLabels, Counter>::default();
        let items_rejected = Family::<SignalLabels, Counter>::default();
        let decode_seconds = Family::<SignalLabels, Histogram>::new_with_constructor(|| {
            Histogram::new(exponential_buckets(1e-5, 4.0, 10))
        });
        let tick_seconds = Histogram::new(exponential_buckets(1e-4, 4.0, 10));
        let snapshot_seconds = Histogram::new(exponential_buckets(1e-3, 4.0, 8));
        let m = Self {
            requests,
            items,
            items_rejected,
            decode_seconds,
            queue_depth: Gauge::default(),
            series: Gauge::default(),
            buffered_spans: Gauge::default(),
            windows: Gauge::default(),
            late: Gauge::default(),
            traces: Gauge::default(),
            incidents_open: Gauge::default(),
            analyses: Gauge::default(),
            tick_seconds,
            snapshot_seconds,
            snapshot_bytes: Gauge::default(),
            deliveries: Family::default(),
            cluster: Family::default(),
            registry: Registry::default(),
        };
        registry.register("ingest_requests", "OTLP export requests", m.requests.clone());
        registry.register("ingest_items", "Spans, data points and log records accepted", m.items.clone());
        registry.register("ingest_items_rejected", "Items rejected as invalid", m.items_rejected.clone());
        registry.register("decode_seconds", "Time spent decoding one request", m.decode_seconds.clone());
        registry.register("queue_depth", "Batches waiting for the engine", m.queue_depth.clone());
        registry.register("series", "Series tracked by the engine", m.series.clone());
        registry.register("buffered_spans", "Spans waiting for their trace to complete", m.buffered_spans.clone());
        registry.register("windows", "Aggregation windows closed", m.windows.clone());
        registry.register("late_observations", "Observations dropped because their window had closed", m.late.clone());
        registry.register("traces", "Traces analysed", m.traces.clone());
        registry.register("incidents_open", "Incidents currently open", m.incidents_open.clone());
        registry.register("analyses", "Root-cause analyses run", m.analyses.clone());
        registry.register("tick_seconds", "Duration of one engine tick", m.tick_seconds.clone());
        registry.register("snapshot_seconds", "Duration of a state snapshot", m.snapshot_seconds.clone());
        registry.register("snapshot_bytes", "Size of the last state snapshot", m.snapshot_bytes.clone());
        registry.register("webhook_deliveries", "Webhook deliveries", m.deliveries.clone());
        registry.register(
            "cluster_summaries",
            "Window summaries shipped by an edge or received by a core",
            m.cluster.clone(),
        );
        Self { registry, ..m }
    }

    /// Renders the OpenMetrics text exposition.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        if prometheus_client::encoding::text::encode(&mut out, &self.registry).is_err() {
            out.clear();
        }
        out
    }

    /// Sets a distributed-mode counter.
    pub fn cluster_set(&self, event: &'static str, value: u64) {
        self.cluster.get_or_create(&ClusterLabels { event }).set(value);
    }

    /// Counts one request.
    pub fn request(&self, signal: &'static str, transport: &'static str, outcome: &'static str) {
        self.requests.get_or_create(&RequestLabels { signal, transport, outcome }).inc();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_registered_metrics() {
        let m = Metrics::new();
        m.request("traces", "grpc", "ok");
        m.items.get_or_create(&SignalLabels { signal: "traces" }).inc_by(10);
        m.series.set(42);
        let text = m.render();
        assert!(
            text.contains("etio_ingest_requests_total{signal=\"traces\",transport=\"grpc\",outcome=\"ok\"} 1"),
            "{text}"
        );
        assert!(text.contains("etio_series 42"));
        assert!(text.contains("# EOF"));
    }
}
