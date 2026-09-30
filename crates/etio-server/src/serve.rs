//! Server assembly and lifecycle.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use etio_engine::Engine;
use etio_otlp::proto::collector::logs::v1::raw::logs_service_server::LogsServiceServer;
use etio_otlp::proto::collector::metrics::v1::raw::metrics_service_server::MetricsServiceServer;
use etio_otlp::proto::collector::trace::v1::raw::trace_service_server::TraceServiceServer;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tonic::codec::CompressionEncoding;

use crate::actor::{Clock, EngineHandle, Mode, wall_now};
use crate::api::{self, ApiState};
use crate::auth::{Auth, GrpcAuth};
use crate::config::{Role, Secret, ServerConfig};
use crate::metrics::Metrics;
use crate::otlp::{self, Receiver};
use crate::persist::{self, Store};
use crate::{cluster, notify, tls};

/// Addresses the server actually bound (useful with port 0).
#[derive(Clone, Debug, Default)]
pub struct Bound {
    /// OTLP/gRPC.
    pub otlp_grpc: Option<SocketAddr>,
    /// OTLP/HTTP.
    pub otlp_http: Option<SocketAddr>,
    /// API.
    pub api: Option<SocketAddr>,
    /// Summary receiver (core role).
    pub cluster: Option<SocketAddr>,
}

/// A running server.
pub struct Running {
    /// Bound addresses.
    pub bound: Bound,
    /// Engine handle (for in-process use and tests).
    pub engine: EngineHandle,
    shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
    engine_thread: std::thread::JoinHandle<Engine>,
    store: Option<Arc<Store>>,
}

/// Loads the engine, restoring the last snapshot when there is one.
fn load_engine(cfg: &ServerConfig, store: Option<&Store>) -> anyhow::Result<Engine> {
    if let Some(store) = store {
        let path = store.snapshot_path();
        match persist::read_snapshot(&path) {
            Ok(Some(snap)) => match Engine::restore(cfg.engine.clone(), snap) {
                Ok(engine) => {
                    tracing::info!(series = engine.store().len(), "restored engine state from {}", path.display());
                    return Ok(engine);
                }
                Err(e) => tracing::warn!(error = %e, "snapshot is incompatible with the configuration; starting fresh"),
            },
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "ignoring unreadable snapshot"),
        }
    }
    Engine::new(cfg.engine.clone()).context("creating the engine")
}

impl Running {
    /// Starts every configured listener and background task.
    ///
    /// # Errors
    /// Fails if a listener cannot bind or a configured file cannot be read.
    pub async fn start(cfg: ServerConfig, clock: Clock) -> anyhow::Result<Self> {
        crate::init_crypto();
        let role = cfg.cluster.role;
        // An edge holds no durable state: its windows live in the cores.
        let store = match role {
            Role::Edge => None,
            _ => cfg.storage.dir.as_deref().map(Store::open).transpose()?.map(Arc::new),
        };
        let engine = load_engine(&cfg, store.as_deref())?;
        let resume_window = engine.store().last_window().map(|w| w + 1);
        let metrics = Arc::new(Metrics::new());
        let (shutdown, shutdown_rx) = watch::channel(false);
        let mut tasks = Vec::new();
        let mut bound = Bound::default();
        let cluster_token = cfg.cluster.token_file.as_deref().map(Secret::from_file).transpose()?;
        let mode = match role {
            Role::Standalone => Mode::Standalone,
            Role::Core => Mode::Core,
            Role::Edge => {
                let edge_id = cfg.cluster.edge_id.clone().unwrap_or_default();
                // Any value greater than the previous incarnation's works; the
                // start time is one that needs no state.
                let epoch = u64::try_from(wall_now()).unwrap_or(0);
                let queue = Arc::new(cluster::EdgeQueue::new(cluster_outbox(&edge_id, epoch, &cfg)));
                let fwd = cluster::ForwarderConfig {
                    edge_id,
                    resolution_ns: duration_ns(cfg.engine.resolution),
                    retransmit: cfg.cluster.retransmit,
                    token: cluster_token.clone(),
                    tls: cfg.cluster.ca_file.as_deref().map(tls::client_config).transpose()?,
                };
                tasks.extend(cluster::spawn_forwarders(&queue, &cfg.cluster.cores, &fwd, &shutdown_rx));
                Mode::Edge(queue)
            }
        };
        let (handle, engine_thread) =
            EngineHandle::spawn_with_mode(engine, cfg.limits.queue_batches, cfg.limits.tick, clock, metrics, mode);
        if role == Role::Core {
            let res = duration_ns(cfg.engine.resolution);
            let mut collector = etio_engine::cluster::Collector::new(
                res,
                duration_ns(cfg.cluster.deadline),
                duration_ns(cfg.cluster.liveness),
            )
            .with_grace(duration_ns(cfg.cluster.grace));
            if let Some(w) = resume_window {
                collector.start_at(w);
            }
            let collector = Arc::new(Mutex::new(collector));
            let addr = cfg.cluster.listen;
            let listener =
                TcpListener::bind(addr).await.with_context(|| format!("binding the summary receiver on {addr}"))?;
            let grpc_tls = cfg.tls.as_ref().map(tls::grpc_config).transpose()?;
            let (local, task) = cluster::spawn_receiver(
                listener,
                cluster::CoreService::new(collector.clone(), res, shutdown_rx.clone()),
                cluster::ClusterAuth(cluster_token.map(Arc::new)),
                grpc_tls,
                shutdown_rx.clone(),
            )?;
            bound.cluster = Some(local);
            tasks.push(task);
            tasks.push(cluster::spawn_releaser(collector, handle.clone(), shutdown_rx.clone()));
            tracing::info!(addr = %local, "core: summary receiver listening");
        }
        let auth = Arc::new(Auth::from_config(&cfg.auth)?);
        let tls_cfg = cfg.tls.as_ref().map(tls::server_config).transpose()?;
        let receiver = Receiver::new(handle.clone(), cfg.limits.max_decompressed_bytes);

        // A core receives summaries, not telemetry.
        let (otlp_grpc, otlp_http) = match role {
            Role::Core => (None, None),
            _ => (cfg.listen.otlp_grpc, cfg.listen.otlp_http),
        };
        if let Some(addr) = otlp_grpc {
            let listener = TcpListener::bind(addr).await.with_context(|| format!("binding OTLP/gRPC on {addr}"))?;
            bound.otlp_grpc = Some(listener.local_addr()?);
            let interceptor = GrpcAuth(auth.clone());
            let limit = cfg.limits.max_decompressed_bytes;
            let traces = TraceServiceServer::new(receiver.clone())
                .accept_compressed(CompressionEncoding::Gzip)
                .accept_compressed(CompressionEncoding::Zstd)
                .max_decoding_message_size(limit);
            let metrics_svc = MetricsServiceServer::new(receiver.clone())
                .accept_compressed(CompressionEncoding::Gzip)
                .accept_compressed(CompressionEncoding::Zstd)
                .max_decoding_message_size(limit);
            let logs = LogsServiceServer::new(receiver.clone())
                .accept_compressed(CompressionEncoding::Gzip)
                .accept_compressed(CompressionEncoding::Zstd)
                .max_decoding_message_size(limit);
            let mut builder = tonic::transport::Server::builder();
            if let Some(t) = &cfg.tls {
                builder = builder.tls_config(tls::grpc_config(t)?)?;
            }
            let router = builder
                .add_service(tonic::service::interceptor::InterceptedService::new(traces, interceptor.clone()))
                .add_service(tonic::service::interceptor::InterceptedService::new(metrics_svc, interceptor.clone()))
                .add_service(tonic::service::interceptor::InterceptedService::new(logs, interceptor));
            let mut rx = shutdown_rx.clone();
            let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
            tasks.push(tokio::spawn(async move {
                let signal = async move {
                    let _ = rx.changed().await;
                };
                if let Err(e) = router.serve_with_incoming_shutdown(incoming, signal).await {
                    tracing::error!(error = %e, "OTLP/gRPC server failed");
                }
            }));
            tracing::info!(addr = %bound.otlp_grpc.unwrap_or(addr), "OTLP/gRPC listening");
        }

        if let Some(addr) = otlp_http {
            let listener = TcpListener::bind(addr).await.with_context(|| format!("binding OTLP/HTTP on {addr}"))?;
            bound.otlp_http = Some(listener.local_addr()?);
            let router = otlp::http_router(receiver.clone(), cfg.limits.max_request_bytes)
                .route_layer(axum::middleware::from_fn_with_state(auth.clone(), crate::auth::require_ingest));
            tasks.push(spawn_http(listener, router, tls_cfg.clone(), shutdown_rx.clone()));
            tracing::info!(addr = %bound.otlp_http.unwrap_or(addr), "OTLP/HTTP listening");
        }

        if let Some(addr) = cfg.listen.api {
            let listener = TcpListener::bind(addr).await.with_context(|| format!("binding the API on {addr}"))?;
            bound.api = Some(listener.local_addr()?);
            let state = ApiState { engine: handle.clone(), store: store.clone(), auth: auth.clone(), role };
            let router = api::router(state, cfg.ui.dir.clone());
            tasks.push(spawn_http(listener, router, tls_cfg.clone(), shutdown_rx.clone()));
            tracing::info!(addr = %bound.api.unwrap_or(addr), "API listening");
        }

        if let Some(store) = &store {
            let (h, s, every) = (handle.clone(), store.clone(), cfg.storage.snapshot_interval);
            tasks.push(tokio::spawn(persist::run(h, s, every)));
        }
        if role != Role::Edge {
            notify::spawn(handle.clone(), cfg.notify.webhooks.clone())?;
        }

        Ok(Self { bound, engine: handle, shutdown, tasks, engine_thread, store })
    }

    /// Stops accepting requests, drains the engine queue, writes a final
    /// snapshot and returns once everything has stopped.
    ///
    /// # Errors
    /// Fails if the final snapshot cannot be written.
    pub async fn stop(self) -> anyhow::Result<()> {
        let _ = self.shutdown.send(true);
        for t in self.tasks {
            t.abort();
            let _ = t.await;
        }
        if let Some(store) = &self.store {
            match persist::snapshot_now(&self.engine, store).await {
                Ok(bytes) => tracing::info!(bytes, "final snapshot written"),
                Err(e) => tracing::error!(error = %e, "final snapshot failed"),
            }
        }
        self.engine.shutdown();
        let thread = self.engine_thread;
        tokio::task::spawn_blocking(move || thread.join())
            .await?
            .map_err(|_| anyhow::anyhow!("engine thread panicked"))?;
        Ok(())
    }
}

fn duration_ns(d: std::time::Duration) -> i64 {
    i64::try_from(d.as_nanos()).unwrap_or(i64::MAX)
}

fn cluster_outbox(edge_id: &str, epoch: u64, cfg: &ServerConfig) -> etio_engine::cluster::Outbox {
    etio_engine::cluster::Outbox::new(edge_id, epoch, cfg.cluster.cores.len(), cfg.cluster.outbox_capacity)
}

fn spawn_http(
    listener: TcpListener,
    router: axum::Router,
    tls: Option<Arc<rustls::ServerConfig>>,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        match tls {
            Some(config) => tls::serve(listener, router, config, shutdown).await,
            None => {
                let mut rx = shutdown;
                let signal = async move {
                    let _ = rx.changed().await;
                };
                if let Err(e) = axum::serve(listener, router).with_graceful_shutdown(signal).await {
                    tracing::error!(error = %e, "HTTP server failed");
                }
            }
        }
    })
}
