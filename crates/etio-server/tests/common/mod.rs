//! Helpers shared by the integration tests.
#![allow(clippy::unwrap_used, dead_code)]

use std::time::Duration;

use etio_core::rng::Rng;
use etio_otlp::proto::collector::trace::v1::ExportTraceServiceRequest;
use etio_otlp::proto::common::v1::{AnyValue, KeyValue, any_value::Value};
use etio_otlp::proto::resource::v1::Resource;
use etio_otlp::proto::trace::v1::{ResourceSpans, ScopeSpans, Span};
use etio_server::config::ServerConfig;

pub const MS: u64 = 1_000_000;
pub const SEC: u64 = 1_000_000_000;
pub const T0: u64 = 1_700_000_000 * SEC;

pub fn loopback() -> Option<std::net::SocketAddr> {
    Some("127.0.0.1:0".parse().unwrap())
}

pub fn config(dir: Option<&std::path::Path>) -> ServerConfig {
    etio_server::init_crypto();
    let mut cfg = ServerConfig::default();
    cfg.listen.otlp_grpc = loopback();
    cfg.listen.otlp_http = loopback();
    cfg.listen.api = loopback();
    cfg.engine.resolution = Duration::from_secs(1);
    cfg.engine.lateness = Duration::from_secs(3);
    cfg.engine.trace_timeout = Duration::from_secs(1);
    cfg.engine.retention = Duration::from_secs(40 * 60);
    cfg.engine.incident.reference = Duration::from_secs(8 * 60);
    cfg.engine.incident.rca_delay = Duration::from_secs(15);
    cfg.engine.incident.resolve_after = Duration::from_secs(60);
    cfg.limits.max_decompressed_bytes = 1 << 20;
    cfg.limits.max_request_bytes = 1 << 20;
    cfg.storage.dir = dir.map(Into::into);
    cfg
}

pub fn kv(k: &str, v: &str) -> KeyValue {
    KeyValue {
        key: k.into(),
        value: Some(AnyValue { value: Some(Value::StringValue(v.into())) }),
        ..Default::default()
    }
}

/// OTLP traces for `frontend -> cart` requests started in `[t, t + 100ms)`.
pub fn traces(rng: &mut Rng, t: u64, cart_extra: u64) -> ExportTraceServiceRequest {
    let mut fe = Vec::new();
    let mut cart = Vec::new();
    for k in 0..2u64 {
        let start = t + k * 50 * MS;
        let mut trace = [0u8; 16];
        trace[..8].copy_from_slice(&rng.next_u64().to_be_bytes());
        trace[8..].copy_from_slice(&rng.next_u64().to_be_bytes());
        let (root, child) = (rng.next_u64().to_be_bytes().to_vec(), rng.next_u64().to_be_bytes().to_vec());
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let own = (rng.lognormal_median_p99(5.0, 3.0) * 1e6) as u64 + cart_extra;
        let cart_start = start + 2 * MS;
        let cart_end = cart_start + own;
        fe.push(Span {
            trace_id: trace.to_vec(),
            span_id: root.clone(),
            name: "GET /".into(),
            kind: 2,
            start_time_unix_nano: start,
            end_time_unix_nano: cart_end + 2 * MS,
            ..Default::default()
        });
        cart.push(Span {
            trace_id: trace.to_vec(),
            span_id: child,
            parent_span_id: root,
            name: "GetCart".into(),
            kind: 2,
            start_time_unix_nano: cart_start,
            end_time_unix_nano: cart_end,
            ..Default::default()
        });
    }
    let rs = |svc: &str, spans: Vec<Span>| ResourceSpans {
        resource: Some(Resource { attributes: vec![kv("service.name", svc)], ..Default::default() }),
        scope_spans: vec![ScopeSpans { spans, ..Default::default() }],
        ..Default::default()
    };
    ExportTraceServiceRequest { resource_spans: vec![rs("frontend", fe), rs("cart", cart)] }
}

pub async fn get_json(client: &reqwest::Client, url: String) -> serde_json::Value {
    client.get(url).send().await.unwrap().error_for_status().unwrap().json().await.unwrap()
}
