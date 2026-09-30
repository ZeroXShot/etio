//! OTLP receivers (gRPC and HTTP).
//!
//! Both transports share [`Receiver::submit`]: decode the request selectively,
//! queue the result for the engine, and translate the outcome into OTLP
//! semantics:
//!
//! | outcome | gRPC | HTTP |
//! |---|---|---|
//! | accepted | `OK` (with `partial_success` if items were rejected) | `200` |
//! | malformed | `INVALID_ARGUMENT` | `400` |
//! | queue full | `UNAVAILABLE` (retryable) | `429` + `Retry-After` |
//! | shutting down | `UNAVAILABLE` | `503` + `Retry-After` |
//!
//! `RESOURCE_EXHAUSTED` is deliberately not used for backpressure: the OTLP
//! specification makes it retryable only when a `RetryInfo` detail is
//! attached, and exporters that do not see one may drop the data.

use std::io::Read;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use etio_otlp::decode::logs::LogOptions;
use etio_otlp::decode::metrics::MetricOptions;
use etio_otlp::decode::traces::TraceOptions;
use etio_otlp::decode::{self, DecodeError};
use etio_otlp::json;
use etio_otlp::proto::collector::logs::v1::{
    ExportLogsPartialSuccess, ExportLogsServiceResponse, raw::logs_service_server,
};
use etio_otlp::proto::collector::metrics::v1::{
    ExportMetricsPartialSuccess, ExportMetricsServiceResponse, raw::metrics_service_server,
};
use etio_otlp::proto::collector::trace::v1::{
    ExportTracePartialSuccess, ExportTraceServiceResponse, raw::trace_service_server,
};
use prost::Message;
use tonic::Status;

use crate::actor::{EngineHandle, Ingest, Rejected};
use crate::metrics::SignalLabels;

/// The kind of telemetry in a request.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    /// Spans.
    Traces,
    /// Metric data points.
    Metrics,
    /// Log records.
    Logs,
}

impl Signal {
    const fn name(self) -> &'static str {
        match self {
            Self::Traces => "traces",
            Self::Metrics => "metrics",
            Self::Logs => "logs",
        }
    }
}

/// Request body encoding.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// Binary protobuf.
    Protobuf,
    /// OTLP/JSON.
    Json,
}

/// Why a request was not accepted.
#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    /// Malformed request.
    #[error(transparent)]
    Invalid(#[from] DecodeError),
    /// Engine busy or stopping.
    #[error(transparent)]
    Rejected(#[from] Rejected),
}

/// Shared receiver state.
#[derive(Clone)]
pub struct Receiver {
    handle: EngineHandle,
    service_namespace: bool,
    max_decompressed: usize,
}

impl Receiver {
    /// Creates a receiver feeding `handle`.
    #[must_use]
    pub const fn new(handle: EngineHandle, max_decompressed: usize) -> Self {
        Self { handle, service_namespace: false, max_decompressed }
    }

    /// Decodes one export request and queues it for the engine. Returns the
    /// number of items rejected as invalid.
    ///
    /// # Errors
    /// Returns [`SubmitError`] when the request is malformed or cannot be queued.
    pub fn submit(
        &self,
        signal: Signal,
        encoding: Encoding,
        body: Bytes,
        transport: &'static str,
    ) -> Result<u64, SubmitError> {
        let metrics = self.handle.metrics().clone();
        let labels = SignalLabels { signal: signal.name() };
        let started = Instant::now();
        let interner = self.handle.interner();
        let ns = self.service_namespace;
        let decoded: Result<(Ingest, decode::DecodeStats), DecodeError> = match (signal, encoding) {
            (Signal::Traces, e) => {
                let mut spans = Vec::new();
                let opts = TraceOptions { service_namespace: ns };
                let stats = match e {
                    Encoding::Protobuf => decode::traces::decode(&body, interner, opts, &mut spans),
                    Encoding::Json => json::decode_traces(&body, interner, opts, &mut spans),
                };
                stats.map(|s| (Ingest::Spans(spans), s))
            }
            (Signal::Metrics, e) => {
                let mut points = Vec::new();
                let opts = MetricOptions { service_namespace: ns };
                let stats = match e {
                    Encoding::Protobuf => decode::metrics::decode(&body, interner, opts, &mut points),
                    Encoding::Json => json::decode_metrics(&body, interner, opts, &mut points),
                };
                stats.map(|s| (Ingest::Metrics(points), s))
            }
            (Signal::Logs, e) => {
                let opts = LogOptions { service_namespace: ns };
                let r = match e {
                    Encoding::Protobuf => decode::logs::decode(body, interner, opts),
                    Encoding::Json => json::decode_logs(&body, interner, opts),
                };
                r.map(|(batch, s)| (Ingest::Logs(batch), s))
            }
        };
        metrics.decode_seconds.get_or_create(&labels).observe(started.elapsed().as_secs_f64());
        let (batch, stats) = match decoded {
            Ok(v) => v,
            Err(e) => {
                metrics.request(signal.name(), transport, "invalid");
                return Err(e.into());
            }
        };
        if let Err(e) = self.handle.try_ingest(batch) {
            metrics.request(signal.name(), transport, "backpressure");
            return Err(e.into());
        }
        metrics.request(signal.name(), transport, "ok");
        metrics.items.get_or_create(&labels).inc_by(stats.accepted);
        metrics.items_rejected.get_or_create(&labels).inc_by(stats.rejected);
        Ok(stats.rejected)
    }

    /// Decompresses a gzip body, refusing to inflate past the configured limit.
    fn inflate(&self, body: &Bytes, headers: &HeaderMap) -> Result<Bytes, (StatusCode, String)> {
        match headers.get(header::CONTENT_ENCODING).and_then(|v| v.to_str().ok()) {
            None | Some("identity") => Ok(body.clone()),
            Some("gzip") => {
                let mut out = Vec::new();
                let limit = u64::try_from(self.max_decompressed).unwrap_or(u64::MAX);
                let mut reader = flate2::read::GzDecoder::new(body.as_ref()).take(limit + 1);
                if reader.read_to_end(&mut out).is_err() {
                    return Err((StatusCode::BAD_REQUEST, "invalid gzip body".into()));
                }
                if out.len() > self.max_decompressed {
                    return Err((StatusCode::PAYLOAD_TOO_LARGE, "decompressed body exceeds the limit".into()));
                }
                Ok(Bytes::from(out))
            }
            Some(other) => Err((StatusCode::UNSUPPORTED_MEDIA_TYPE, format!("unsupported content encoding `{other}`"))),
        }
    }
}

// -- gRPC ------------------------------------------------------------------------------------

fn grpc_status(e: &SubmitError) -> Status {
    match e {
        SubmitError::Invalid(d) => Status::invalid_argument(d.to_string()),
        SubmitError::Rejected(Rejected::Full) => Status::unavailable("ingest queue full; retry with backoff"),
        SubmitError::Rejected(Rejected::Stopped) => Status::unavailable("server shutting down"),
    }
}

#[tonic::async_trait]
impl trace_service_server::TraceService for Receiver {
    async fn export(
        &self,
        request: tonic::Request<Bytes>,
    ) -> Result<tonic::Response<ExportTraceServiceResponse>, Status> {
        let rejected = self
            .submit(Signal::Traces, Encoding::Protobuf, request.into_inner(), "grpc")
            .map_err(|e| grpc_status(&e))?;
        let partial_success = (rejected > 0).then(|| ExportTracePartialSuccess {
            rejected_spans: i64::try_from(rejected).unwrap_or(i64::MAX),
            error_message: "spans without a valid trace or span id were dropped".into(),
        });
        Ok(tonic::Response::new(ExportTraceServiceResponse { partial_success }))
    }
}

#[tonic::async_trait]
impl metrics_service_server::MetricsService for Receiver {
    async fn export(
        &self,
        request: tonic::Request<Bytes>,
    ) -> Result<tonic::Response<ExportMetricsServiceResponse>, Status> {
        let rejected = self
            .submit(Signal::Metrics, Encoding::Protobuf, request.into_inner(), "grpc")
            .map_err(|e| grpc_status(&e))?;
        let partial_success = (rejected > 0).then(|| ExportMetricsPartialSuccess {
            rejected_data_points: i64::try_from(rejected).unwrap_or(i64::MAX),
            error_message: "data points without a value or timestamp were dropped".into(),
        });
        Ok(tonic::Response::new(ExportMetricsServiceResponse { partial_success }))
    }
}

#[tonic::async_trait]
impl logs_service_server::LogsService for Receiver {
    async fn export(
        &self,
        request: tonic::Request<Bytes>,
    ) -> Result<tonic::Response<ExportLogsServiceResponse>, Status> {
        let rejected =
            self.submit(Signal::Logs, Encoding::Protobuf, request.into_inner(), "grpc").map_err(|e| grpc_status(&e))?;
        let partial_success = (rejected > 0).then(|| ExportLogsPartialSuccess {
            rejected_log_records: i64::try_from(rejected).unwrap_or(i64::MAX),
            error_message: "log records without a timestamp or text body were dropped".into(),
        });
        Ok(tonic::Response::new(ExportLogsServiceResponse { partial_success }))
    }
}

// -- HTTP ------------------------------------------------------------------------------------

fn encoding_of(headers: &HeaderMap) -> Option<Encoding> {
    let ct = headers.get(header::CONTENT_TYPE)?.to_str().ok()?;
    let mime = ct.split(';').next()?.trim();
    match mime {
        "application/x-protobuf" | "application/protobuf" => Some(Encoding::Protobuf),
        "application/json" => Some(Encoding::Json),
        _ => None,
    }
}

fn http_response(signal: Signal, encoding: Encoding, rejected: u64) -> Response {
    let rejected_i = i64::try_from(rejected).unwrap_or(i64::MAX);
    let (bytes, json) = match signal {
        Signal::Traces => {
            let r = ExportTraceServiceResponse {
                partial_success: (rejected > 0)
                    .then(|| ExportTracePartialSuccess { rejected_spans: rejected_i, error_message: String::new() }),
            };
            (
                r.encode_to_vec(),
                serde_json::json!({"partialSuccess": (rejected > 0).then(|| serde_json::json!({"rejectedSpans": rejected.to_string()}))}),
            )
        }
        Signal::Metrics => {
            let r = ExportMetricsServiceResponse {
                partial_success: (rejected > 0).then(|| ExportMetricsPartialSuccess {
                    rejected_data_points: rejected_i,
                    error_message: String::new(),
                }),
            };
            (
                r.encode_to_vec(),
                serde_json::json!({"partialSuccess": (rejected > 0).then(|| serde_json::json!({"rejectedDataPoints": rejected.to_string()}))}),
            )
        }
        Signal::Logs => {
            let r = ExportLogsServiceResponse {
                partial_success: (rejected > 0).then(|| ExportLogsPartialSuccess {
                    rejected_log_records: rejected_i,
                    error_message: String::new(),
                }),
            };
            (
                r.encode_to_vec(),
                serde_json::json!({"partialSuccess": (rejected > 0).then(|| serde_json::json!({"rejectedLogRecords": rejected.to_string()}))}),
            )
        }
    };
    match encoding {
        Encoding::Protobuf => ([(header::CONTENT_TYPE, "application/x-protobuf")], bytes).into_response(),
        Encoding::Json => {
            let mut body = json;
            if rejected == 0 {
                body = serde_json::json!({});
            }
            ([(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response()
        }
    }
}

async fn handle_http(receiver: Arc<Receiver>, signal: Signal, headers: HeaderMap, body: Bytes) -> Response {
    let Some(encoding) = encoding_of(&headers) else {
        receiver.handle.metrics().request(signal.name(), "http", "unsupported");
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "expected application/x-protobuf or application/json")
            .into_response();
    };
    let body = match receiver.inflate(&body, &headers) {
        Ok(b) => b,
        Err(err) => {
            receiver.handle.metrics().request(signal.name(), "http", "invalid");
            return err.into_response();
        }
    };
    let retry_after = |status: StatusCode, msg: &'static str| {
        let mut r = (status, msg).into_response();
        r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        r
    };
    match receiver.submit(signal, encoding, body, "http") {
        Ok(rejected) => http_response(signal, encoding, rejected),
        Err(SubmitError::Invalid(e)) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        Err(SubmitError::Rejected(Rejected::Full)) => retry_after(StatusCode::TOO_MANY_REQUESTS, "ingest queue full"),
        Err(SubmitError::Rejected(Rejected::Stopped)) => retry_after(StatusCode::SERVICE_UNAVAILABLE, "shutting down"),
    }
}

/// The OTLP/HTTP router (`/v1/traces`, `/v1/metrics`, `/v1/logs`).
pub fn http_router(receiver: Receiver, max_request_bytes: usize) -> Router {
    let receiver = Arc::new(receiver);
    Router::new()
        .route(
            "/v1/traces",
            post(|State(r): State<Arc<Receiver>>, h: HeaderMap, b: Bytes| handle_http(r, Signal::Traces, h, b)),
        )
        .route(
            "/v1/metrics",
            post(|State(r): State<Arc<Receiver>>, h: HeaderMap, b: Bytes| handle_http(r, Signal::Metrics, h, b)),
        )
        .route(
            "/v1/logs",
            post(|State(r): State<Arc<Receiver>>, h: HeaderMap, b: Bytes| handle_http(r, Signal::Logs, h, b)),
        )
        .layer(DefaultBodyLimit::max(max_request_bytes))
        .with_state(receiver)
}
