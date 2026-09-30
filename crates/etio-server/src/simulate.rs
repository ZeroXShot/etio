//! `etio sim`: drive a server with simulated OpenTelemetry traffic.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use etio_otlp::proto::collector::logs::v1::logs_service_client::LogsServiceClient;
use etio_otlp::proto::collector::metrics::v1::metrics_service_client::MetricsServiceClient;
use etio_otlp::proto::collector::trace::v1::trace_service_client::TraceServiceClient;
use etio_sim::telemetry::to_otlp;
use etio_sim::{Fault, FaultKind, Simulation, Topology, Workload};
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

/// Parses a fault written as `kind[=value]:target@start+duration`, for
/// example `cpu=6:cart@10m+5m`, `crash:payment@15m+3m` or `delay=80:shipping@600+120`.
///
/// # Errors
/// Fails on malformed specifications.
pub fn parse_fault(spec: &str) -> anyhow::Result<Fault> {
    let (kind, rest) = spec.split_once(':').context("expected kind:target@start+duration")?;
    let (target, when) = rest.split_once('@').context("expected target@start+duration")?;
    let (start, duration) = when.split_once('+').context("expected start+duration")?;
    let (name, value) = kind.split_once('=').map_or((kind, None), |(n, v)| (n, Some(v)));
    let value = value.map(str::parse::<f64>).transpose().context("fault value must be a number")?;
    let kind = match name {
        "cpu" => FaultKind::Cpu { factor: value.unwrap_or(6.0) },
        "delay" => FaultKind::Delay { ms: value.unwrap_or(80.0) },
        "loss" => FaultKind::Loss { rate: value.unwrap_or(0.15) },
        "errors" => FaultKind::Errors { rate: value.unwrap_or(0.25) },
        "leak" => FaultKind::MemoryLeak { mb_per_s: value.unwrap_or(4.0) },
        "crash" => FaultKind::Crash,
        other => bail!("unknown fault kind `{other}` (cpu, delay, loss, errors, leak, crash)"),
    };
    let secs = |s: &str| etio_core::time::parse_duration(s).map(|d| d.as_secs_f64()).map_err(anyhow::Error::from);
    Ok(Fault { target: target.to_owned(), kind, start_s: secs(start)?, duration_s: secs(duration)? })
}

/// Parses `shop` or `layered:N[:seed]`.
///
/// # Errors
/// Fails on unknown topologies.
pub fn parse_topology(spec: &str) -> anyhow::Result<Topology> {
    let mut parts = spec.split(':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("shop"), None, None) => Ok(Topology::shop()),
        (Some("layered"), Some(n), seed) => {
            let n = n.parse().context("layered:N needs a number of services")?;
            let seed = seed.map(str::parse).transpose().context("seed must be an integer")?.unwrap_or(1);
            Ok(Topology::layered(n, seed))
        }
        _ => bail!("unknown topology `{spec}` (shop, layered:N[:seed])"),
    }
}

/// Options of [`run`].
pub struct DriveOptions {
    /// OTLP/gRPC endpoint, e.g. `http://127.0.0.1:4317`.
    pub endpoint: String,
    /// The simulated system.
    pub topology: Topology,
    /// Requests per second at the entry points.
    pub rate: f64,
    /// Faults.
    pub faults: Vec<Fault>,
    /// How long to simulate.
    pub duration: Duration,
    /// Simulated seconds per wall-clock second; 0 sends as fast as possible.
    pub speed: f64,
    /// Bearer token for the server, if it requires one.
    pub token: Option<String>,
    /// Seed.
    pub seed: u64,
}

/// Streams simulated telemetry to an OTLP endpoint. Timestamps follow the
/// wall clock in real time (`speed == 1`); otherwise they start now and
/// advance at the simulated pace.
///
/// # Errors
/// Fails if the endpoint is unreachable or rejects the data.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub async fn run(opts: DriveOptions) -> anyhow::Result<()> {
    let channel = Channel::from_shared(opts.endpoint.clone())?
        .connect()
        .await
        .with_context(|| format!("connecting to {}", opts.endpoint))?;
    let auth = opts.token.map(|t| MetadataValue::try_from(format!("Bearer {t}"))).transpose()?;
    let with_auth = move |mut req: tonic::Request<()>| {
        if let Some(a) = &auth {
            req.metadata_mut().insert("authorization", a.clone());
        }
        Ok(req)
    };
    let mut traces = TraceServiceClient::with_interceptor(channel.clone(), with_auth.clone());
    let mut metrics = MetricsServiceClient::with_interceptor(channel.clone(), with_auth.clone());
    let mut logs = LogsServiceClient::with_interceptor(channel, with_auth);

    let workload = Workload { rate: opts.rate, ..Workload::default() };
    let mut sim =
        Simulation::new(opts.topology.clone(), workload, opts.faults, opts.seed).map_err(anyhow::Error::msg)?;
    let t0 = crate::actor::wall_now();
    let started = Instant::now();
    let total = opts.duration.as_secs_f64();
    let (mut spans, mut last_report) = (0usize, Instant::now());
    while sim.time() < total {
        let batch = sim.step(1.0);
        spans += batch.spans.len();
        let req = to_otlp(&batch, &opts.topology, t0);
        traces.export(req.traces).await.context("exporting traces")?;
        if !req.metrics.resource_metrics.is_empty() {
            metrics.export(req.metrics).await.context("exporting metrics")?;
        }
        if !req.logs.resource_logs.is_empty() {
            logs.export(req.logs).await.context("exporting logs")?;
        }
        if opts.speed > 0.0 {
            let due = Duration::from_secs_f64(sim.time() / opts.speed);
            if let Some(wait) = due.checked_sub(started.elapsed()) {
                tokio::time::sleep(wait).await;
            }
        }
        if last_report.elapsed() >= Duration::from_secs(10) {
            let rate = spans as f64 / started.elapsed().as_secs_f64();
            tracing::info!(simulated_s = sim.time() as u64, spans, spans_per_s = rate as u64, "sending");
            last_report = Instant::now();
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    tracing::info!(spans, seconds = elapsed, spans_per_s = (spans as f64 / elapsed) as u64, "done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fault_specifications() {
        let f = parse_fault("cpu=4:cart@10m+5m").unwrap();
        assert_eq!(f.target, "cart");
        assert_eq!(f.kind, FaultKind::Cpu { factor: 4.0 });
        assert!((f.start_s - 600.0).abs() < 1e-9 && (f.duration_s - 300.0).abs() < 1e-9);
        assert_eq!(parse_fault("crash:payment@60+30").unwrap().kind, FaultKind::Crash);
        for bad in ["cpu", "cpu:cart", "cpu:cart@1m", "nope:cart@1m+1m", "cpu=x:cart@1m+1m"] {
            assert!(parse_fault(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_topologies() {
        assert_eq!(parse_topology("shop").unwrap().services.len(), 11);
        assert_eq!(parse_topology("layered:25").unwrap().services.len(), 25);
        assert!(parse_topology("mesh").is_err());
    }
}
