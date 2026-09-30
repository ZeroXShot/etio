//! OTLP/JSON decoding.
//!
//! OTLP/JSON follows the protobuf JSON mapping with OTLP-specific rules:
//! trace and span identifiers are hexadecimal strings, 64-bit integers may be
//! strings or numbers, enums are integers, and keys are lowerCamelCase (the
//! original snake_case names are accepted too).
//!
//! JSON is parsed into the generated `prost` types, re-encoded, and handed to
//! the selective protobuf decoders. There is therefore exactly one place that
//! defines what a span, a data point or a log record means; JSON is a thin
//! syntactic layer on top. Re-encoding costs little next to JSON parsing, and
//! JSON is the minority encoding in OTLP deployments.

use bytes::Bytes;
use etio_core::Interner;
use etio_engine::MetricPoint;
use etio_pipeline::Span;
use prost::Message;
use serde::Deserialize;
use serde::de::{self, Deserializer};

use crate::decode::logs::{LogBatch, LogOptions};
use crate::decode::metrics::MetricOptions;
use crate::decode::traces::TraceOptions;
use crate::decode::{DecodeError, DecodeStats};
use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
use crate::proto::collector::metrics::v1::ExportMetricsServiceRequest;
use crate::proto::collector::trace::v1::ExportTraceServiceRequest;
use crate::proto::common::v1 as pc;
use crate::proto::logs::v1 as pl;
use crate::proto::metrics::v1 as pm;
use crate::proto::resource::v1 as pr;
use crate::proto::trace::v1 as pt;

fn json_err(e: impl std::fmt::Display) -> DecodeError {
    DecodeError::Json(e.to_string())
}

/// A 64-bit integer written as a JSON number or string.
fn u64_any<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum N {
        U(u64),
        S(String),
        F(f64),
    }
    match N::deserialize(d)? {
        N::U(v) => Ok(v),
        N::S(s) if s.is_empty() => Ok(0),
        N::S(s) => s.trim().parse().map_err(de::Error::custom),
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        N::F(f) if f >= 0.0 && f.fract() == 0.0 => Ok(f as u64),
        N::F(f) => Err(de::Error::custom(format!("not an unsigned integer: {f}"))),
    }
}

fn i64_any<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum N {
        I(i64),
        S(String),
    }
    Ok(match Option::<N>::deserialize(d)? {
        Some(N::I(v)) => Some(v),
        Some(N::S(s)) => Some(s.trim().parse().map_err(de::Error::custom)?),
        None => None,
    })
}

/// A double written as a number or as one of the protobuf JSON strings.
fn f64_any<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum N {
        F(f64),
        S(String),
    }
    Ok(match Option::<N>::deserialize(d)? {
        Some(N::F(v)) => Some(v),
        Some(N::S(s)) => Some(match s.as_str() {
            "NaN" => f64::NAN,
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            other => other.parse().map_err(de::Error::custom)?,
        }),
        None => None,
    })
}

/// An enum written as an integer (OTLP) or as its name (protobuf JSON).
fn enum_any<'de, D: Deserializer<'de>>(d: D) -> Result<i32, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum E {
        I(i32),
        S(String),
    }
    Ok(match E::deserialize(d)? {
        E::I(v) => v,
        E::S(s) => match s.as_str() {
            "SPAN_KIND_INTERNAL" | "STATUS_CODE_OK" | "AGGREGATION_TEMPORALITY_DELTA" => 1,
            "SPAN_KIND_SERVER" | "STATUS_CODE_ERROR" | "AGGREGATION_TEMPORALITY_CUMULATIVE" => 2,
            "SPAN_KIND_CLIENT" => 3,
            "SPAN_KIND_PRODUCER" => 4,
            "SPAN_KIND_CONSUMER" => 5,
            other => other.parse().unwrap_or(0),
        },
    })
}

fn hex_id(s: &str) -> Vec<u8> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return Vec::new();
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .unwrap_or_default()
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct AnyValueJson {
    #[serde(alias = "string_value")]
    string_value: Option<String>,
    #[serde(alias = "bool_value")]
    bool_value: Option<bool>,
    #[serde(alias = "int_value", deserialize_with = "i64_any")]
    int_value: Option<i64>,
    #[serde(alias = "double_value", deserialize_with = "f64_any")]
    double_value: Option<f64>,
    #[serde(alias = "kvlist_value")]
    kvlist_value: Option<KvListJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct KvListJson {
    values: Vec<KeyValueJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct KeyValueJson {
    key: String,
    value: Option<AnyValueJson>,
}

impl From<AnyValueJson> for pc::AnyValue {
    fn from(v: AnyValueJson) -> Self {
        use pc::any_value::Value;
        let value = if let Some(s) = v.string_value {
            Some(Value::StringValue(s))
        } else if let Some(b) = v.bool_value {
            Some(Value::BoolValue(b))
        } else if let Some(i) = v.int_value {
            Some(Value::IntValue(i))
        } else if let Some(d) = v.double_value {
            Some(Value::DoubleValue(d))
        } else {
            v.kvlist_value.map(|l| Value::KvlistValue(pc::KeyValueList { values: kvs(l.values) }))
        };
        Self { value }
    }
}

fn kvs(v: Vec<KeyValueJson>) -> Vec<pc::KeyValue> {
    v.into_iter()
        .map(|kv| pc::KeyValue { key: kv.key, value: kv.value.map(Into::into), ..Default::default() })
        .collect()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ResourceJson {
    attributes: Vec<KeyValueJson>,
}

impl From<ResourceJson> for pr::Resource {
    fn from(r: ResourceJson) -> Self {
        Self { attributes: kvs(r.attributes), ..Default::default() }
    }
}

// -- traces ---------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct TracesJson {
    #[serde(alias = "resource_spans")]
    resource_spans: Vec<ResourceSpansJson>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ResourceSpansJson {
    resource: Option<ResourceJson>,
    #[serde(alias = "scope_spans")]
    scope_spans: Vec<ScopeSpansJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ScopeSpansJson {
    spans: Vec<SpanJson>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct SpanJson {
    #[serde(alias = "trace_id")]
    trace_id: String,
    #[serde(alias = "span_id")]
    span_id: String,
    #[serde(alias = "parent_span_id")]
    parent_span_id: String,
    name: String,
    #[serde(deserialize_with = "enum_any")]
    kind: i32,
    #[serde(alias = "start_time_unix_nano", deserialize_with = "u64_any")]
    start_time_unix_nano: u64,
    #[serde(alias = "end_time_unix_nano", deserialize_with = "u64_any")]
    end_time_unix_nano: u64,
    attributes: Vec<KeyValueJson>,
    status: Option<StatusJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StatusJson {
    #[serde(deserialize_with = "enum_any")]
    code: i32,
}

/// Decodes an OTLP/JSON trace export request.
///
/// # Errors
/// Returns [`DecodeError::Json`] if the document is not valid OTLP/JSON.
pub fn decode_traces(
    json: &[u8],
    interner: &Interner,
    opts: TraceOptions,
    out: &mut Vec<Span>,
) -> Result<DecodeStats, DecodeError> {
    let doc: TracesJson = serde_json::from_slice(json).map_err(json_err)?;
    let req = ExportTraceServiceRequest {
        resource_spans: doc
            .resource_spans
            .into_iter()
            .map(|rs| pt::ResourceSpans {
                resource: rs.resource.map(Into::into),
                scope_spans: rs
                    .scope_spans
                    .into_iter()
                    .map(|ss| pt::ScopeSpans {
                        spans: ss
                            .spans
                            .into_iter()
                            .map(|s| pt::Span {
                                trace_id: hex_id(&s.trace_id),
                                span_id: hex_id(&s.span_id),
                                parent_span_id: hex_id(&s.parent_span_id),
                                name: s.name,
                                kind: s.kind,
                                start_time_unix_nano: s.start_time_unix_nano,
                                end_time_unix_nano: s.end_time_unix_nano,
                                attributes: kvs(s.attributes),
                                status: s.status.map(|st| pt::Status { code: st.code, ..Default::default() }),
                                ..Default::default()
                            })
                            .collect(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            })
            .collect(),
    };
    crate::decode::traces::decode(&req.encode_to_vec(), interner, opts, out)
}

// -- metrics --------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct MetricsJson {
    #[serde(alias = "resource_metrics")]
    resource_metrics: Vec<ResourceMetricsJson>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ResourceMetricsJson {
    resource: Option<ResourceJson>,
    #[serde(alias = "scope_metrics")]
    scope_metrics: Vec<ScopeMetricsJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ScopeMetricsJson {
    metrics: Vec<MetricJson>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct MetricJson {
    name: String,
    gauge: Option<PointsJson>,
    sum: Option<PointsJson>,
    histogram: Option<PointsJson>,
    #[serde(alias = "exponential_histogram")]
    exponential_histogram: Option<PointsJson>,
    summary: Option<PointsJson>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct PointsJson {
    #[serde(alias = "data_points")]
    data_points: Vec<PointJson>,
    #[serde(alias = "aggregation_temporality", deserialize_with = "enum_any")]
    aggregation_temporality: i32,
    #[serde(alias = "is_monotonic")]
    is_monotonic: bool,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct PointJson {
    attributes: Vec<KeyValueJson>,
    #[serde(alias = "start_time_unix_nano", deserialize_with = "u64_any")]
    start_time_unix_nano: u64,
    #[serde(alias = "time_unix_nano", deserialize_with = "u64_any")]
    time_unix_nano: u64,
    #[serde(alias = "as_double", deserialize_with = "f64_any")]
    as_double: Option<f64>,
    #[serde(alias = "as_int", deserialize_with = "i64_any")]
    as_int: Option<i64>,
    #[serde(deserialize_with = "u64_any")]
    count: u64,
    #[serde(deserialize_with = "f64_any")]
    sum: Option<f64>,
    #[serde(alias = "quantile_values")]
    quantile_values: Vec<QuantileJson>,
    flags: u32,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct QuantileJson {
    #[serde(deserialize_with = "f64_any")]
    quantile: Option<f64>,
    #[serde(deserialize_with = "f64_any")]
    value: Option<f64>,
}

fn number_points(points: Vec<PointJson>) -> Vec<pm::NumberDataPoint> {
    points
        .into_iter()
        .map(|p| pm::NumberDataPoint {
            attributes: kvs(p.attributes),
            start_time_unix_nano: p.start_time_unix_nano,
            time_unix_nano: p.time_unix_nano,
            value: p
                .as_double
                .map(pm::number_data_point::Value::AsDouble)
                .or(p.as_int.map(pm::number_data_point::Value::AsInt)),
            flags: p.flags,
            ..Default::default()
        })
        .collect()
}

fn metric_data(m: MetricJson) -> Option<pm::metric::Data> {
    use pm::metric::Data;
    if let Some(g) = m.gauge {
        return Some(Data::Gauge(pm::Gauge { data_points: number_points(g.data_points) }));
    }
    if let Some(s) = m.sum {
        return Some(Data::Sum(pm::Sum {
            data_points: number_points(s.data_points),
            aggregation_temporality: s.aggregation_temporality,
            is_monotonic: s.is_monotonic,
        }));
    }
    if let Some(h) = m.histogram.or(m.exponential_histogram) {
        return Some(Data::Histogram(pm::Histogram {
            data_points: h
                .data_points
                .into_iter()
                .map(|p| pm::HistogramDataPoint {
                    attributes: kvs(p.attributes),
                    start_time_unix_nano: p.start_time_unix_nano,
                    time_unix_nano: p.time_unix_nano,
                    count: p.count,
                    sum: p.sum,
                    flags: p.flags,
                    ..Default::default()
                })
                .collect(),
            aggregation_temporality: h.aggregation_temporality,
        }));
    }
    m.summary.map(|s| {
        Data::Summary(pm::Summary {
            data_points: s
                .data_points
                .into_iter()
                .map(|p| pm::SummaryDataPoint {
                    attributes: kvs(p.attributes),
                    start_time_unix_nano: p.start_time_unix_nano,
                    time_unix_nano: p.time_unix_nano,
                    count: p.count,
                    sum: p.sum.unwrap_or(0.0),
                    quantile_values: p
                        .quantile_values
                        .into_iter()
                        .map(|q| pm::summary_data_point::ValueAtQuantile {
                            quantile: q.quantile.unwrap_or(0.0),
                            value: q.value.unwrap_or(f64::NAN),
                        })
                        .collect(),
                    flags: p.flags,
                })
                .collect(),
        })
    })
}

/// Decodes an OTLP/JSON metrics export request.
///
/// # Errors
/// Returns [`DecodeError::Json`] if the document is not valid OTLP/JSON.
pub fn decode_metrics(
    json: &[u8],
    interner: &Interner,
    opts: MetricOptions,
    out: &mut Vec<MetricPoint>,
) -> Result<DecodeStats, DecodeError> {
    let doc: MetricsJson = serde_json::from_slice(json).map_err(json_err)?;
    let req = ExportMetricsServiceRequest {
        resource_metrics: doc
            .resource_metrics
            .into_iter()
            .map(|rm| pm::ResourceMetrics {
                resource: rm.resource.map(Into::into),
                scope_metrics: rm
                    .scope_metrics
                    .into_iter()
                    .map(|sm| pm::ScopeMetrics {
                        metrics: sm
                            .metrics
                            .into_iter()
                            .map(|m| pm::Metric { name: m.name.clone(), data: metric_data(m), ..Default::default() })
                            .collect(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            })
            .collect(),
    };
    crate::decode::metrics::decode(&req.encode_to_vec(), interner, opts, out)
}

// -- logs -----------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct LogsJson {
    #[serde(alias = "resource_logs")]
    resource_logs: Vec<ResourceLogsJson>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ResourceLogsJson {
    resource: Option<ResourceJson>,
    #[serde(alias = "scope_logs")]
    scope_logs: Vec<ScopeLogsJson>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ScopeLogsJson {
    #[serde(alias = "log_records")]
    log_records: Vec<LogRecordJson>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct LogRecordJson {
    #[serde(alias = "time_unix_nano", deserialize_with = "u64_any")]
    time_unix_nano: u64,
    #[serde(alias = "observed_time_unix_nano", deserialize_with = "u64_any")]
    observed_time_unix_nano: u64,
    #[serde(alias = "severity_number", deserialize_with = "enum_any")]
    severity_number: i32,
    body: Option<AnyValueJson>,
}

/// Decodes an OTLP/JSON logs export request.
///
/// # Errors
/// Returns [`DecodeError::Json`] if the document is not valid OTLP/JSON.
pub fn decode_logs(json: &[u8], interner: &Interner, opts: LogOptions) -> Result<(LogBatch, DecodeStats), DecodeError> {
    let doc: LogsJson = serde_json::from_slice(json).map_err(json_err)?;
    let req = ExportLogsServiceRequest {
        resource_logs: doc
            .resource_logs
            .into_iter()
            .map(|rl| pl::ResourceLogs {
                resource: rl.resource.map(Into::into),
                scope_logs: rl
                    .scope_logs
                    .into_iter()
                    .map(|sl| pl::ScopeLogs {
                        log_records: sl
                            .log_records
                            .into_iter()
                            .map(|r| pl::LogRecord {
                                time_unix_nano: r.time_unix_nano,
                                observed_time_unix_nano: r.observed_time_unix_nano,
                                severity_number: r.severity_number,
                                body: r.body.map(Into::into),
                                ..Default::default()
                            })
                            .collect(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            })
            .collect(),
    };
    crate::decode::logs::decode(Bytes::from(req.encode_to_vec()), interner, opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use etio_pipeline::{SpanKind, SpanStatus};

    #[test]
    fn decodes_the_specification_example() {
        // Shape of the example in the OTLP specification.
        let json = br#"{
          "resourceSpans": [{
            "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "my.service"}}]},
            "scopeSpans": [{
              "scope": {"name": "my.library", "version": "1.0.0"},
              "spans": [{
                "traceId": "5B8EFFF798038103D269B633813FC60C",
                "spanId": "EEE19B7EC3C1B174",
                "parentSpanId": "EEE19B7EC3C1B173",
                "name": "I'm a server span",
                "startTimeUnixNano": "1544712660000000000",
                "endTimeUnixNano": 1544712661000000000,
                "kind": 2,
                "attributes": [{"key": "http.response.status_code", "value": {"intValue": "503"}}]
              }]
            }]
          }]
        }"#;
        let i = Interner::default();
        let mut out = Vec::new();
        let stats = decode_traces(json, &i, TraceOptions::default(), &mut out).unwrap();
        assert_eq!(stats.accepted, 1);
        let s = &out[0];
        assert_eq!(s.trace_id, 0x5B8E_FFF7_9803_8103_D269_B633_813F_C60C);
        assert_eq!(s.span_id, 0xEEE1_9B7E_C3C1_B174);
        assert_eq!(s.parent_id, 0xEEE1_9B7E_C3C1_B173);
        assert_eq!(s.kind, SpanKind::Server);
        assert_eq!(s.status, SpanStatus::Error, "5xx on a server span");
        assert_eq!(s.end - s.start, 1_000_000_000);
        assert_eq!(&*i.resolve(s.service), "my.service");
    }

    #[test]
    fn accepts_snake_case_and_enum_names() {
        let json = br#"{"resource_spans": [{"scope_spans": [{"spans": [{
            "trace_id": "0102030405060708090a0b0c0d0e0f10", "span_id": "0102030405060708",
            "kind": "SPAN_KIND_CLIENT", "status": {"code": "STATUS_CODE_ERROR"}}]}]}]}"#;
        let i = Interner::default();
        let mut out = Vec::new();
        decode_traces(json, &i, TraceOptions::default(), &mut out).unwrap();
        assert_eq!(out[0].kind, SpanKind::Client);
        assert_eq!(out[0].status, SpanStatus::Error);
    }

    #[test]
    fn decodes_metrics_and_logs() {
        let i = Interner::default();
        let metrics = br#"{"resourceMetrics": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "cart"}}]},
            "scopeMetrics": [{"metrics": [
              {"name": "requests", "sum": {"aggregationTemporality": 2, "isMonotonic": true,
                 "dataPoints": [{"asInt": "42", "timeUnixNano": "10", "startTimeUnixNano": "1"}]}},
              {"name": "latency", "histogram": {"aggregationTemporality": 1,
                 "dataPoints": [{"count": "5", "sum": 1.5, "timeUnixNano": "10"}]}}
            ]}]}]}"#;
        let mut points = Vec::new();
        let stats = decode_metrics(metrics, &i, MetricOptions::default(), &mut points).unwrap();
        assert_eq!(stats.accepted, 2);
        assert_eq!(points.len(), 3);
        assert!((points[0].value - 42.0).abs() < 1e-12);

        let logs = br#"{"resourceLogs": [{"scopeLogs": [{"logRecords": [
            {"timeUnixNano": "7", "severityNumber": 17, "body": {"stringValue": "boom"}}]}]}]}"#;
        let (batch, _) = decode_logs(logs, &i, LogOptions::default()).unwrap();
        assert_eq!(batch.entries().next().unwrap().body, "boom");
    }

    #[test]
    fn rejects_malformed_json() {
        let i = Interner::default();
        let mut out = Vec::new();
        assert!(decode_traces(b"{not json", &i, TraceOptions::default(), &mut out).is_err());
        assert!(decode_traces(br#"{"resourceSpans": 3}"#, &i, TraceOptions::default(), &mut out).is_err());
    }
}
