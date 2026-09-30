//! Integration tests: a real server on ephemeral ports, real OTLP clients.
#![allow(clippy::unwrap_used)]

use std::io::Write;

mod common;

use common::{MS, SEC, T0, config, get_json, traces};
use etio_core::rng::Rng;
use etio_otlp::proto::collector::trace::v1::trace_service_client::TraceServiceClient;
use etio_server::actor::Clock;
use etio_server::serve::Running;
use prost::Message;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_to_end_over_otlp_grpc_with_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let server = Running::start(config(Some(dir.path())), Clock::Manual).await.unwrap();
    let grpc = format!("http://{}", server.bound.otlp_grpc.unwrap());
    let api = format!("http://{}", server.bound.api.unwrap());
    let mut client = TraceServiceClient::connect(grpc).await.unwrap();
    let http = reqwest::Client::new();
    let mut events = server.engine.subscribe();

    // 12 minutes of normal traffic, then 2 minutes with a slow cart.
    let mut rng = Rng::seed_from_u64(1);
    let mut t = T0;
    while t < T0 + 840 * SEC {
        let extra = if (T0 + 600 * SEC..T0 + 720 * SEC).contains(&t) { 60 * MS } else { 0 };
        let resp = client.export(traces(&mut rng, t, extra)).await.unwrap().into_inner();
        assert!(resp.partial_success.is_none());
        t += 100 * MS;
        server.engine.advance(i64::try_from(t).unwrap()).unwrap();
    }
    // Let the engine drain the queue before querying.
    let status = get_json(&http, format!("{api}/api/v1/status")).await;
    assert!(status["series"].as_u64().unwrap() >= 12, "{status}");

    let services = get_json(&http, format!("{api}/api/v1/services")).await;
    let names: Vec<&str> = services.as_array().unwrap().iter().map(|s| s["service"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["cart", "frontend"]);

    let graph = get_json(&http, format!("{api}/api/v1/graph")).await;
    assert_eq!(graph["edges"][0][0], "frontend");
    assert_eq!(graph["edges"][0][1], "cart");

    let values = get_json(&http, format!("{api}/api/v1/series/values?service=cart&name=trace_local_p95")).await;
    assert!(values["values"].as_array().unwrap().iter().any(|v| v.as_f64().is_some_and(|x| x > 50.0)));

    let incidents = get_json(&http, format!("{api}/api/v1/incidents")).await;
    let list = incidents.as_array().unwrap();
    assert_eq!(list.len(), 1, "{incidents}");
    assert_eq!(list[0]["top"][0], "cart");
    let id = list[0]["id"].as_str().unwrap().to_owned();
    let detail = get_json(&http, format!("{api}/api/v1/incidents/{id}")).await;
    assert_eq!(detail["rca"]["ranking"][0]["service"], "cart");

    // The event stream saw the incident.
    let mut saw_open = false;
    while let Ok(e) = events.try_recv() {
        saw_open |= matches!(&*e, etio_engine::Event::IncidentOpened { .. });
    }
    assert!(saw_open);

    // Feedback is stored.
    let r = http
        .post(format!("{api}/api/v1/incidents/{id}/feedback"))
        .json(&serde_json::json!({"root_cause": "cart", "comment": "confirmed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);

    // On-demand analysis at the fault start.
    let fault = (T0 + 600 * SEC) as f64 / 1e9;
    let r: serde_json::Value = http
        .post(format!("{api}/api/v1/analyze"))
        .json(&serde_json::json!({"anomaly_time": fault}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["ranking"][0]["service"], "cart");

    let metrics = http.get(format!("{api}/metrics")).send().await.unwrap().text().await.unwrap();
    assert!(
        metrics.contains("etio_ingest_requests_total{signal=\"traces\",transport=\"grpc\",outcome=\"ok\"} 8400"),
        "{metrics}"
    );

    // Restart from the snapshot: history and incidents survive.
    server.stop().await.unwrap();
    let server = Running::start(config(Some(dir.path())), Clock::Manual).await.unwrap();
    let api = format!("http://{}", server.bound.api.unwrap());
    let status = get_json(&http, format!("{api}/api/v1/status")).await;
    assert!(status["series"].as_u64().unwrap() >= 12);
    let detail = get_json(&http, format!("{api}/api/v1/incidents/{id}")).await;
    assert_eq!(detail["id"], id.as_str());
    server.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otlp_http_encodings_and_errors() {
    let server = Running::start(config(None), Clock::Manual).await.unwrap();
    let base = format!("http://{}", server.bound.otlp_http.unwrap());
    let http = reqwest::Client::new();
    let mut rng = Rng::seed_from_u64(2);
    let body = traces(&mut rng, T0, 0).encode_to_vec();

    // Protobuf.
    let r = http
        .post(format!("{base}/v1/traces"))
        .header("content-type", "application/x-protobuf")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "application/x-protobuf");

    // Gzip-compressed protobuf.
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&body).unwrap();
    let r = http
        .post(format!("{base}/v1/traces"))
        .header("content-type", "application/x-protobuf")
        .header("content-encoding", "gzip")
        .body(gz.finish().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    // JSON with an invalid span (partial success).
    let json = serde_json::json!({"resourceSpans": [{"scopeSpans": [{"spans": [
        {"traceId": "0102030405060708090a0b0c0d0e0f10", "spanId": "0102030405060708", "startTimeUnixNano": "1", "endTimeUnixNano": "2"},
        {"traceId": "", "spanId": ""}
    ]}]}]});
    let r = http.post(format!("{base}/v1/traces")).json(&json).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["partialSuccess"]["rejectedSpans"], "1");

    // Errors.
    let r = http.post(format!("{base}/v1/traces")).header("content-type", "text/plain").body("x").send().await.unwrap();
    assert_eq!(r.status(), 415);
    let r = http
        .post(format!("{base}/v1/traces"))
        .header("content-type", "application/x-protobuf")
        .body(vec![0x0a, 0xff])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    // A gzip bomb: 4 MiB of zeros compress to a few KiB but exceed the 1 MiB limit.
    let mut bomb = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    bomb.write_all(&vec![0u8; 4 << 20]).unwrap();
    let r = http
        .post(format!("{base}/v1/logs"))
        .header("content-type", "application/x-protobuf")
        .header("content-encoding", "gzip")
        .body(bomb.finish().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    server.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bearer_tokens_protect_ingest_and_api() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ingest"), "in-token\n").unwrap();
    std::fs::write(dir.path().join("read"), "read-token\n").unwrap();
    let mut cfg = config(None);
    cfg.auth.ingest_token_file = Some(dir.path().join("ingest"));
    cfg.auth.read_token_file = Some(dir.path().join("read"));
    let server = Running::start(cfg, Clock::Manual).await.unwrap();
    let http = reqwest::Client::new();
    let otlp = format!("http://{}/v1/traces", server.bound.otlp_http.unwrap());
    let api = format!("http://{}", server.bound.api.unwrap());
    let body = traces(&mut Rng::seed_from_u64(3), T0, 0).encode_to_vec();
    let send = |token: Option<&'static str>| {
        let mut r = http.post(&otlp).header("content-type", "application/x-protobuf").body(body.clone());
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        r.send()
    };
    assert_eq!(send(None).await.unwrap().status(), 401);
    assert_eq!(send(Some("read-token")).await.unwrap().status(), 401);
    assert_eq!(send(Some("in-token")).await.unwrap().status(), 200);

    assert_eq!(http.get(format!("{api}/api/v1/status")).send().await.unwrap().status(), 401);
    assert_eq!(http.get(format!("{api}/api/v1/status")).bearer_auth("read-token").send().await.unwrap().status(), 200);
    assert_eq!(http.get(format!("{api}/healthz")).send().await.unwrap().status(), 200, "liveness stays open");

    // gRPC without the token is rejected.
    let mut client = TraceServiceClient::connect(format!("http://{}", server.bound.otlp_grpc.unwrap())).await.unwrap();
    let err = client.export(traces(&mut Rng::seed_from_u64(4), T0, 0)).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
    server.stop().await.unwrap();
}
