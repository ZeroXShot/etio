//! Converting simulated telemetry into engine records or OTLP requests.

use etio_core::{Interner, Sym};
use etio_engine::{MetricKind, MetricPoint};
use etio_otlp::proto::collector::logs::v1::ExportLogsServiceRequest;
use etio_otlp::proto::collector::metrics::v1::ExportMetricsServiceRequest;
use etio_otlp::proto::collector::trace::v1::ExportTraceServiceRequest;
use etio_otlp::proto::common::v1::{AnyValue, KeyValue, any_value::Value};
use etio_otlp::proto::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use etio_otlp::proto::metrics::v1::{
    Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, metric, number_data_point,
};
use etio_otlp::proto::resource::v1::Resource;
use etio_otlp::proto::trace::v1::{ResourceSpans, ScopeSpans, Span as OtlpSpan, Status};
use etio_pipeline::logs::Severity;
use etio_pipeline::{Span, SpanKind, SpanStatus};

use crate::sim::Batch;
use crate::topology::Topology;

#[allow(clippy::cast_possible_truncation)]
fn ns(t0: i64, t: f64) -> i64 {
    t0 + (t * 1e9).round() as i64
}

/// Records ready for [`etio_engine::Engine`] ingestion.
#[derive(Debug, Default)]
pub struct EngineRecords {
    /// Spans.
    pub spans: Vec<Span>,
    /// Metric points.
    pub metrics: Vec<MetricPoint>,
    /// Log records: timestamp, service, body, severity.
    pub logs: Vec<(i64, Sym, String, Option<Severity>)>,
}

/// Converts a batch into engine records, interning names in `interner`.
#[must_use]
pub fn to_engine(batch: &Batch, topology: &Topology, interner: &Interner, t0: i64) -> EngineRecords {
    let names: Vec<Sym> = topology.services.iter().map(|s| interner.intern(&s.name)).collect();
    let spans = batch
        .spans
        .iter()
        .map(|s| Span {
            trace_id: s.trace_id,
            span_id: s.span_id,
            parent_id: s.parent_id,
            service: names[s.service],
            operation: interner.intern(&s.name),
            kind: if s.server { SpanKind::Server } else { SpanKind::Client },
            start: ns(t0, s.start),
            end: ns(t0, s.end),
            status: if s.error { SpanStatus::Error } else { SpanStatus::Unset },
            peer: s.db.as_deref().or(s.peer.as_deref()).map_or(Sym::EMPTY, |d| interner.intern(d)),
        })
        .collect();
    let metrics = batch
        .metrics
        .iter()
        .map(|m| MetricPoint {
            service: names[m.service],
            name: interner.intern(m.name),
            stream: (m.service as u64) << 8 | u64::from(m.name.len() as u8),
            ts: ns(t0, m.t),
            value: m.value,
            kind: MetricKind::Gauge,
        })
        .collect();
    let logs = batch
        .logs
        .iter()
        .map(|l| {
            (
                ns(t0, l.t),
                names[l.service],
                l.body.clone(),
                Some(if l.error { Severity::Error } else { Severity::Info }),
            )
        })
        .collect();
    EngineRecords { spans, metrics, logs }
}

fn resource(name: &str) -> Resource {
    Resource {
        attributes: vec![KeyValue {
            key: "service.name".into(),
            value: Some(AnyValue { value: Some(Value::StringValue(name.into())) }),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// One OTLP export request per signal.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct OtlpRequests {
    /// Traces.
    pub traces: ExportTraceServiceRequest,
    /// Metrics.
    pub metrics: ExportMetricsServiceRequest,
    /// Logs.
    pub logs: ExportLogsServiceRequest,
}

/// Converts a batch into OTLP requests, one resource per service.
#[must_use]
#[allow(clippy::cast_sign_loss)]
pub fn to_otlp(batch: &Batch, topology: &Topology, t0: i64) -> OtlpRequests {
    let n = topology.services.len();
    let mut spans: Vec<Vec<OtlpSpan>> = vec![Vec::new(); n];
    for s in &batch.spans {
        let mut attributes = Vec::new();
        let mut attr = |k: &str, v: &str| {
            attributes.push(KeyValue {
                key: k.into(),
                value: Some(AnyValue { value: Some(Value::StringValue(v.into())) }),
                ..Default::default()
            });
        };
        if let Some(db) = &s.db {
            attr("db.system.name", db);
        } else if let Some(peer) = &s.peer {
            attr("peer.service", peer);
        }
        spans[s.service].push(OtlpSpan {
            trace_id: s.trace_id.to_be_bytes().to_vec(),
            span_id: s.span_id.to_be_bytes().to_vec(),
            parent_span_id: if s.parent_id == 0 { Vec::new() } else { s.parent_id.to_be_bytes().to_vec() },
            name: s.name.clone(),
            kind: if s.server { 2 } else { 3 },
            start_time_unix_nano: ns(t0, s.start) as u64,
            end_time_unix_nano: ns(t0, s.end) as u64,
            attributes,
            status: s.error.then(|| Status { code: 2, ..Default::default() }),
            ..Default::default()
        });
    }
    let traces = ExportTraceServiceRequest {
        resource_spans: spans
            .into_iter()
            .enumerate()
            .filter(|(_, v)| !v.is_empty())
            .map(|(i, spans)| ResourceSpans {
                resource: Some(resource(&topology.services[i].name)),
                scope_spans: vec![ScopeSpans { spans, ..Default::default() }],
                ..Default::default()
            })
            .collect(),
    };

    let mut metrics: Vec<Vec<Metric>> = vec![Vec::new(); n];
    for m in &batch.metrics {
        metrics[m.service].push(Metric {
            name: m.name.into(),
            data: Some(metric::Data::Gauge(Gauge {
                data_points: vec![NumberDataPoint {
                    time_unix_nano: ns(t0, m.t) as u64,
                    value: Some(number_data_point::Value::AsDouble(m.value)),
                    ..Default::default()
                }],
            })),
            ..Default::default()
        });
    }
    let metrics = ExportMetricsServiceRequest {
        resource_metrics: metrics
            .into_iter()
            .enumerate()
            .filter(|(_, v)| !v.is_empty())
            .map(|(i, metrics)| ResourceMetrics {
                resource: Some(resource(&topology.services[i].name)),
                scope_metrics: vec![ScopeMetrics { metrics, ..Default::default() }],
                ..Default::default()
            })
            .collect(),
    };

    let mut logs: Vec<Vec<LogRecord>> = vec![Vec::new(); n];
    for l in &batch.logs {
        logs[l.service].push(LogRecord {
            time_unix_nano: ns(t0, l.t) as u64,
            severity_number: if l.error { 17 } else { 9 },
            body: Some(AnyValue { value: Some(Value::StringValue(l.body.clone())) }),
            ..Default::default()
        });
    }
    let logs = ExportLogsServiceRequest {
        resource_logs: logs
            .into_iter()
            .enumerate()
            .filter(|(_, v)| !v.is_empty())
            .map(|(i, log_records)| ResourceLogs {
                resource: Some(resource(&topology.services[i].name)),
                scope_logs: vec![ScopeLogs { log_records, ..Default::default() }],
                ..Default::default()
            })
            .collect(),
    };
    OtlpRequests { traces, metrics, logs }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{Simulation, Workload};
    use etio_otlp::decode::traces::{TraceOptions, decode};
    use prost::Message;

    #[test]
    fn otlp_and_engine_conversions_agree() {
        let mut sim = Simulation::new(Topology::shop(), Workload::default(), vec![], 3).unwrap();
        let batch = sim.step(1.0);
        let t0 = 1_700_000_000_000_000_000;
        let interner = Interner::default();
        let direct = to_engine(&batch, sim.topology(), &interner, t0);
        let otlp = to_otlp(&batch, sim.topology(), t0);
        let mut decoded = Vec::new();
        decode(&otlp.traces.encode_to_vec(), &interner, TraceOptions::default(), &mut decoded).unwrap();
        let key = |s: &Span| s.span_id;
        let mut a = direct.spans;
        let mut b = decoded;
        a.sort_by_key(key);
        b.sort_by_key(key);
        assert_eq!(a, b, "the OTLP path and the in-process path produce the same spans");
        assert!(!otlp.metrics.resource_metrics.is_empty());
    }
}
