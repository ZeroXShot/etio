//! Distributed mode: two edges and a core, real gRPC streams on ephemeral ports.
#![allow(clippy::unwrap_used)]

mod common;

use std::time::{Duration, Instant};

use common::{MS, SEC, T0, config, get_json, loopback, traces};
use etio_core::rng::Rng;
use etio_otlp::proto::collector::trace::v1::trace_service_client::TraceServiceClient;
use etio_server::actor::Clock;
use etio_server::config::{Role, ServerConfig};
use etio_server::serve::Running;

fn core_config() -> ServerConfig {
    let mut cfg = config(None);
    cfg.cluster.role = Role::Core;
    cfg.cluster.listen = loopback().unwrap();
    // Edges may drift apart by a minute of event time; the test feeds them
    // far faster than real time, so this absorbs scheduling jitter.
    cfg.cluster.deadline = Duration::from_secs(60);
    cfg.cluster.grace = Duration::from_millis(300);
    cfg
}

fn edge_config(id: &str, core: std::net::SocketAddr) -> ServerConfig {
    let mut cfg = config(None);
    cfg.cluster.role = Role::Edge;
    cfg.cluster.edge_id = Some(id.into());
    cfg.cluster.cores = vec![format!("http://{core}")];
    cfg.cluster.retransmit = Duration::from_secs(1);
    cfg
}

async fn wait_for(what: &str, timeout: Duration, mut done: impl AsyncFnMut() -> bool) {
    let start = Instant::now();
    while !done().await {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The same scenario as the single-node end-to-end test, with each trace
/// sent to one of two edges: the core must see the merged traffic and find
/// the same incident.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_edges_and_a_core_find_the_incident() {
    let core = Running::start(core_config(), Clock::Manual).await.unwrap();
    assert!(core.bound.otlp_grpc.is_none(), "a core receives no telemetry");
    let core_addr = core.bound.cluster.unwrap();
    let edges = [
        Running::start(edge_config("edge-0", core_addr), Clock::Manual).await.unwrap(),
        Running::start(edge_config("edge-1", core_addr), Clock::Manual).await.unwrap(),
    ];
    let mut clients = Vec::new();
    for e in &edges {
        clients.push(TraceServiceClient::connect(format!("http://{}", e.bound.otlp_grpc.unwrap())).await.unwrap());
    }
    let http = reqwest::Client::new();
    let api = format!("http://{}", core.bound.api.unwrap());
    // Let both edges connect before any window can close.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut rng = Rng::seed_from_u64(1);
    let mut t = T0;
    let mut k = 0usize;
    while t < T0 + 840 * SEC {
        let extra = if (T0 + 600 * SEC..T0 + 720 * SEC).contains(&t) { 60 * MS } else { 0 };
        clients[k % 2].export(traces(&mut rng, t, extra)).await.unwrap();
        k += 1;
        t += 100 * MS;
        for e in &edges {
            e.engine.advance(i64::try_from(t).unwrap()).unwrap();
        }
    }
    // Windows up to the edges' watermark (now minus the lateness) are sealed.
    wait_for("the core to apply every window", Duration::from_secs(60), async || {
        let status = get_json(&http, format!("{api}/api/v1/status")).await;
        status["stats"]["windows"].as_u64().unwrap() >= 836
    })
    .await;

    let services = get_json(&http, format!("{api}/api/v1/services")).await;
    let names: Vec<&str> = services.as_array().unwrap().iter().map(|s| s["service"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["cart", "frontend"]);
    let graph = get_json(&http, format!("{api}/api/v1/graph")).await;
    assert_eq!(graph["edges"][0][0], "frontend");
    assert_eq!(graph["edges"][0][1], "cart");

    // Both edges' traffic was merged: 2 traces per request, 10 requests per second.
    let values = get_json(&http, format!("{api}/api/v1/series/values?service=frontend&name=trace_requests")).await;
    let rates: Vec<f64> = values["values"].as_array().unwrap().iter().filter_map(serde_json::Value::as_f64).collect();
    let median = {
        let mut r = rates.clone();
        r.sort_by(f64::total_cmp);
        r[r.len() / 2]
    };
    assert!((median - 20.0).abs() < 1e-9, "merged request rate {median} (both edges)");

    let incidents = get_json(&http, format!("{api}/api/v1/incidents")).await;
    let list = incidents.as_array().unwrap();
    assert_eq!(list.len(), 1, "{incidents}");
    assert_eq!(list[0]["top"][0], "cart");

    let metrics = http.get(format!("{api}/metrics")).send().await.unwrap().text().await.unwrap();
    let accepted = metrics
        .lines()
        .find(|l| l.starts_with("etio_cluster_summaries{event=\"accepted\"}"))
        .and_then(|l| l.rsplit(' ').next()?.parse::<u64>().ok())
        .unwrap();
    assert!(accepted >= 2 * 836, "{metrics}");
    assert!(metrics.contains("etio_cluster_summaries{event=\"late\"} 0"), "{metrics}");
    assert!(metrics.contains("etio_cluster_summaries{event=\"lost\"} 0"), "{metrics}");

    for e in edges {
        e.stop().await.unwrap();
    }
    core.stop().await.unwrap();
}

/// An edge buffers while its core is down and delivers everything once the
/// core is back; the core keeps what it had.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edge_survives_a_core_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut core_cfg = core_config();
    core_cfg.storage.dir = Some(dir.path().into());
    let core = Running::start(core_cfg.clone(), Clock::Manual).await.unwrap();
    let core_addr = core.bound.cluster.unwrap();
    let edge = Running::start(edge_config("edge-0", core_addr), Clock::Manual).await.unwrap();
    let mut client = TraceServiceClient::connect(format!("http://{}", edge.bound.otlp_grpc.unwrap())).await.unwrap();
    let http = reqwest::Client::new();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut rng = Rng::seed_from_u64(2);
    let mut t = T0;
    let mut send_until = async |end: u64| {
        while t < end {
            client.export(traces(&mut rng, t, 0)).await.unwrap();
            t += 100 * MS;
            edge.engine.advance(i64::try_from(t).unwrap()).unwrap();
        }
    };
    send_until(T0 + 60 * SEC).await;
    let api = format!("http://{}", core.bound.api.unwrap());
    wait_for("the first windows", Duration::from_secs(30), async || {
        get_json(&http, format!("{api}/api/v1/status")).await["stats"]["windows"].as_u64().unwrap() >= 50
    })
    .await;
    core.stop().await.unwrap();

    // The core is down: the edge keeps sealing windows into its outbox.
    send_until(T0 + 120 * SEC).await;

    // Same address, restored from its snapshot.
    core_cfg.cluster.listen = core_addr;
    let core = Running::start(core_cfg, Clock::Manual).await.unwrap();
    let api = format!("http://{}", core.bound.api.unwrap());
    send_until(T0 + 130 * SEC).await;
    wait_for("the buffered windows", Duration::from_secs(60), async || {
        let status = get_json(&http, format!("{api}/api/v1/status")).await;
        status["stats"]["windows"].as_u64().unwrap() >= 120
    })
    .await;
    let metrics = http.get(format!("{api}/metrics")).send().await.unwrap().text().await.unwrap();
    assert!(metrics.contains("etio_cluster_summaries{event=\"lost\"} 0"), "{metrics}");
    edge.stop().await.unwrap();
    core.stop().await.unwrap();
}
