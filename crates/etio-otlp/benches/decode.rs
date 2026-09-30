//! Selective decoding versus full `prost` decoding of a realistic OTLP trace
//! export: 512 spans with typical HTTP attributes, events and a rich resource.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use etio_core::Interner;
use etio_core::rng::Rng;
use etio_otlp::decode::traces::{TraceOptions, decode};
use etio_otlp::proto::collector::trace::v1::ExportTraceServiceRequest;
use etio_otlp::proto::common::v1::{AnyValue, KeyValue, any_value::Value};
use etio_otlp::proto::resource::v1::Resource;
use etio_otlp::proto::trace::v1::{ResourceSpans, ScopeSpans, Span, Status, span};
use prost::Message;
use std::hint::black_box;

fn kv(k: &str, v: Value) -> KeyValue {
    KeyValue { key: k.into(), value: Some(AnyValue { value: Some(v) }), ..Default::default() }
}

fn s(v: &str) -> Value {
    Value::StringValue(v.into())
}

fn request(spans: usize) -> Vec<u8> {
    let mut rng = Rng::seed_from_u64(1);
    let resource = Resource {
        attributes: vec![
            kv("service.name", s("checkout")),
            kv("service.version", s("1.42.0")),
            kv("service.instance.id", s("checkout-7d9f8b6c5-x2k4p")),
            kv("k8s.pod.name", s("checkout-7d9f8b6c5-x2k4p")),
            kv("k8s.namespace.name", s("shop")),
            kv("k8s.deployment.name", s("checkout")),
            kv("host.name", s("ip-10-0-12-34")),
            kv("telemetry.sdk.language", s("go")),
            kv("telemetry.sdk.name", s("opentelemetry")),
            kv("telemetry.sdk.version", s("1.30.0")),
        ],
        ..Default::default()
    };
    let spans = (0..spans)
        .map(|i| {
            let mut id = [0u8; 16];
            id[..8].copy_from_slice(&rng.next_u64().to_be_bytes());
            id[8..].copy_from_slice(&rng.next_u64().to_be_bytes());
            let start = 1_700_000_000_000_000_000u64 + (i as u64) * 1_000_000;
            Span {
                trace_id: id.to_vec(),
                span_id: rng.next_u64().to_be_bytes().to_vec(),
                parent_span_id: rng.next_u64().to_be_bytes().to_vec(),
                name: "POST /api/checkout".into(),
                kind: if i % 2 == 0 { 2 } else { 3 },
                start_time_unix_nano: start,
                end_time_unix_nano: start + rng.below(50_000_000),
                attributes: vec![
                    kv("http.request.method", s("POST")),
                    kv("http.route", s("/api/checkout")),
                    kv("url.path", s("/api/checkout")),
                    kv("url.scheme", s("https")),
                    kv("http.response.status_code", Value::IntValue(200)),
                    kv("server.address", s("checkout.shop.svc.cluster.local")),
                    kv("server.port", Value::IntValue(8080)),
                    kv("network.protocol.version", s("1.1")),
                    kv("user_agent.original", s("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")),
                    kv("client.address", s("10.0.3.17")),
                ],
                events: vec![span::Event {
                    name: "cart.validated".into(),
                    time_unix_nano: start + 10,
                    ..Default::default()
                }],
                status: Some(Status { code: 0, ..Default::default() }),
                ..Default::default()
            }
        })
        .collect();
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(resource),
            scope_spans: vec![ScopeSpans { spans, ..Default::default() }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

fn bench(c: &mut Criterion) {
    let buf = request(512);
    let mut g = c.benchmark_group("otlp_traces_512_spans");
    g.throughput(Throughput::Elements(512));
    g.bench_function("prost_full_decode", |b| {
        b.iter(|| ExportTraceServiceRequest::decode(black_box(buf.as_slice())).expect("valid"));
    });
    let interner = Interner::default();
    let mut out = Vec::with_capacity(512);
    g.bench_function("etio_selective_decode", |b| {
        b.iter(|| {
            out.clear();
            decode(black_box(&buf), &interner, TraceOptions::default(), &mut out).expect("valid")
        });
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
