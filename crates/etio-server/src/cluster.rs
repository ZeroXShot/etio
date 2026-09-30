//! Distributed deployment: edge forwarders and the core summary receiver.
//!
//! The protocol logic (sequencing, retention, deduplication, ordered
//! release) lives in [`etio_engine::cluster`] as pure state machines tested
//! by deterministic simulation; this module is the transport around them.
//! The simulation also dictates three transport rules implemented here:
//! a core does not release windows during a start-up grace period, an edge
//! rewinds to the acknowledged sequence when the core reports a gap, and an
//! edge replays from the last acknowledgement when acknowledgements stop
//! making progress (the retransmission timer).

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use etio_engine::WindowSummary;
use etio_engine::cluster::{Accept, Collector, Outbox};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tonic::metadata::MetadataValue;
use tonic::{Request, Response, Status, Streaming};

use crate::actor::{EngineHandle, Ingest, Rejected, wall_now};
use crate::config::Secret;

/// Generated protocol types (`proto/etio/cluster/v1/cluster.proto`).
#[allow(missing_docs, clippy::all, clippy::pedantic, unused_qualifications, rust_2018_idioms)]
pub mod proto {
    tonic::include_proto!("etio.cluster.v1");
}

use proto::summaries_client::SummariesClient;
use proto::summaries_server::{Summaries, SummariesServer};
use proto::{Batch, Hello, PushRequest, PushResponse, push_request};

/// Encoding version of `Batch.summary`.
pub const SUMMARY_FORMAT: u32 = 2;

/// Summaries in flight per core before waiting for acknowledgements.
const MAX_IN_FLIGHT: u64 = 256;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// An edge's outbox, shared by the engine thread (which fills it) and the
/// forwarders (which drain it).
pub struct EdgeQueue {
    outbox: Mutex<Outbox>,
    ready: tokio::sync::Notify,
}

impl EdgeQueue {
    /// Wraps an outbox.
    #[must_use]
    pub fn new(outbox: Outbox) -> Self {
        Self { outbox: Mutex::new(outbox), ready: tokio::sync::Notify::new() }
    }

    /// Enqueues sealed summaries and wakes the forwarders.
    pub fn push(&self, summaries: Vec<WindowSummary>) -> etio_engine::cluster::OutboxStats {
        let mut ob = lock(&self.outbox);
        let any = !summaries.is_empty();
        for s in summaries {
            ob.push(s);
        }
        let stats = ob.stats();
        drop(ob);
        if any {
            self.ready.notify_waiters();
        }
        stats
    }

    /// Summaries retained.
    #[must_use]
    pub fn len(&self) -> usize {
        lock(&self.outbox).len()
    }

    /// Whether nothing is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// -- edge ---------------------------------------------------------------------------------

/// Settings of the edge forwarders.
#[derive(Clone)]
pub struct ForwarderConfig {
    /// Edge identity.
    pub edge_id: String,
    /// Window width, ns.
    pub resolution_ns: i64,
    /// Replay from the last acknowledgement after this long without progress.
    pub retransmit: Duration,
    /// Token presented to the cores.
    pub token: Option<Secret>,
    /// Trust roots for `https://` cores.
    pub tls: Option<tonic::transport::ClientTlsConfig>,
}

/// Starts one forwarder per core. Each keeps a stream open to its core,
/// reconnecting with exponential backoff and jitter.
#[must_use]
pub fn spawn_forwarders(
    queue: &Arc<EdgeQueue>,
    cores: &[String],
    cfg: &ForwarderConfig,
    shutdown: &watch::Receiver<bool>,
) -> Vec<JoinHandle<()>> {
    cores
        .iter()
        .enumerate()
        .map(|(index, endpoint)| {
            let (queue, endpoint, cfg, mut shutdown) = (queue.clone(), endpoint.clone(), cfg.clone(), shutdown.clone());
            tokio::spawn(async move {
                let mut backoff = Duration::from_millis(250);
                let mut jitter = etio_core::rng::Rng::seed_from_u64(u64::try_from(index).unwrap_or(0) ^ 0xed6e);
                loop {
                    match session(index, &endpoint, &queue, &cfg, &mut shutdown).await {
                        Ok(()) => return,
                        Err(e) => tracing::warn!(core = %endpoint, error = %e, "summary stream interrupted"),
                    }
                    let wait = backoff.mul_f64(jitter.uniform(0.5, 1.5));
                    tokio::select! {
                        () = tokio::time::sleep(wait) => {}
                        _ = shutdown.changed() => return,
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            })
        })
        .collect()
}

async fn session(
    core: usize,
    endpoint: &str,
    queue: &EdgeQueue,
    cfg: &ForwarderConfig,
    shutdown: &mut watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut ep = tonic::transport::Endpoint::from_shared(endpoint.to_owned())?
        .connect_timeout(Duration::from_secs(5))
        .http2_keep_alive_interval(Duration::from_secs(20));
    if let Some(tls) = &cfg.tls {
        ep = ep.tls_config(tls.clone())?;
    }
    let channel = ep.connect().await?;
    let auth = cfg.token.as_ref().map(|t| MetadataValue::try_from(format!("Bearer {}", t.expose()))).transpose()?;
    let mut client = SummariesClient::with_interceptor(channel, move |mut req: Request<()>| {
        if let Some(a) = &auth {
            req.metadata_mut().insert("authorization", a.clone());
        }
        Ok(req)
    });
    let (tx, rx) = mpsc::channel::<PushRequest>(64);
    let (epoch, first_seq) = {
        let ob = lock(&queue.outbox);
        (ob.epoch(), ob.first_unacked(core))
    };
    tx.send(PushRequest {
        message: Some(push_request::Message::Hello(Hello {
            edge_id: cfg.edge_id.clone(),
            epoch,
            resolution_ns: cfg.resolution_ns,
            summary_format: SUMMARY_FORMAT,
            version: env!("CARGO_PKG_VERSION").into(),
            first_seq,
        })),
    })
    .await?;
    let mut inbound = client.push(ReceiverStream::new(rx)).await?.into_inner();
    tracing::info!(core = %endpoint, "summary stream open");

    let mut cursor = lock(&queue.outbox).acked(core) + 1;
    let mut last_ack = cursor - 1;
    let mut progress = Instant::now();
    loop {
        // Send what the core has not seen, within the in-flight window.
        let batch: Vec<Batch> = {
            let ob = lock(&queue.outbox);
            let limit = ob.acked(core) + MAX_IN_FLIGHT;
            ob.unacked(core)
                .filter(|e| e.seq >= cursor && e.seq <= limit)
                .take(64)
                .filter_map(|e| {
                    Some(Batch { seq: e.seq, window: e.summary.window, summary: postcard::to_stdvec(&e.summary).ok()? })
                })
                .collect()
        };
        for b in batch {
            cursor = b.seq + 1;
            tx.send(PushRequest { message: Some(push_request::Message::Batch(b)) }).await?;
        }
        tokio::select! {
            msg = inbound.message() => {
                let Some(resp) = msg? else { anyhow::bail!("core closed the stream") };
                if !resp.error.is_empty() {
                    anyhow::bail!("core refused the stream: {}", resp.error);
                }
                lock(&queue.outbox).ack(core, resp.acked);
                if resp.acked > last_ack {
                    last_ack = resp.acked;
                    progress = Instant::now();
                }
                if resp.gap && lock(&queue.outbox).first_unacked(core) > resp.acked + 1 {
                    // The outbox overflowed while the core was away: declare
                    // the loss in a new hello instead of replaying forever.
                    anyhow::bail!("summaries after {} were dropped by the outbox; resynchronising", resp.acked);
                }
                if resp.gap || resp.acked >= cursor {
                    cursor = resp.acked + 1;
                }
            }
            () = queue.ready.notified() => {}
            () = tokio::time::sleep(Duration::from_millis(500)) => {}
            _ = shutdown.changed() => return Ok(()),
        }
        let stalled = lock(&queue.outbox).unacked(core).next().is_some() && progress.elapsed() > cfg.retransmit;
        if stalled {
            cursor = lock(&queue.outbox).acked(core) + 1;
            progress = Instant::now();
        }
    }
}

// -- core ---------------------------------------------------------------------------------

/// The core's summary receiver.
pub struct CoreService {
    collector: Arc<Mutex<Collector>>,
    resolution_ns: i64,
    shutdown: watch::Receiver<bool>,
}

impl CoreService {
    /// Creates the service around a shared collector. Open streams end when
    /// `shutdown` fires, so that edges reconnect to the next incarnation
    /// instead of being acknowledged by a stopping one.
    #[must_use]
    pub const fn new(collector: Arc<Mutex<Collector>>, resolution_ns: i64, shutdown: watch::Receiver<bool>) -> Self {
        Self { collector, resolution_ns, shutdown }
    }
}

type PushStream = Pin<Box<dyn Stream<Item = Result<PushResponse, Status>> + Send>>;

fn refuse(error: String) -> PushResponse {
    PushResponse { acked: 0, gap: false, error }
}

#[tonic::async_trait]
impl Summaries for CoreService {
    type PushStream = PushStream;

    async fn push(&self, request: Request<Streaming<PushRequest>>) -> Result<Response<PushStream>, Status> {
        let mut inbound = request.into_inner();
        let first = inbound.message().await?.ok_or_else(|| Status::invalid_argument("empty stream"))?;
        let Some(push_request::Message::Hello(hello)) = first.message else {
            return Err(Status::invalid_argument("the first message must be a hello"));
        };
        let (tx, rx) = mpsc::channel::<Result<PushResponse, Status>>(64);
        if hello.resolution_ns != self.resolution_ns || hello.summary_format != SUMMARY_FORMAT {
            let msg = format!(
                "incompatible edge: resolution {} ns / format {} (core: {} ns / format {SUMMARY_FORMAT})",
                hello.resolution_ns, hello.summary_format, self.resolution_ns
            );
            let _ = tx.send(Ok(refuse(msg))).await;
            return Ok(Response::new(Box::pin(ReceiverStream::new(rx))));
        }
        let collector = self.collector.clone();
        let (edge, epoch) = (hello.edge_id, hello.epoch);
        let resume = lock(&collector).hello(&edge, epoch, hello.first_seq, wall_now());
        tracing::info!(%edge, epoch, resume, "edge connected");
        let _ = tx.send(Ok(PushResponse { acked: resume, gap: false, error: String::new() })).await;
        let mut shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            loop {
                let msg = tokio::select! {
                    m = inbound.next() => m,
                    _ = shutdown.changed() => None,
                };
                let Some(Ok(PushRequest { message: Some(push_request::Message::Batch(b)) })) = msg else { break };
                let summary: WindowSummary = match postcard::from_bytes(&b.summary) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(Ok(refuse(format!("undecodable summary: {e}")))).await;
                        break;
                    }
                };
                let reply = match lock(&collector).accept(&edge, epoch, b.seq, summary, wall_now()) {
                    Accept::Ack(a) => PushResponse { acked: a, gap: false, error: String::new() },
                    Accept::Gap(a) => PushResponse { acked: a, gap: true, error: String::new() },
                    Accept::Stale => refuse("superseded epoch".into()),
                };
                if tx.send(Ok(reply)).await.is_err() {
                    break;
                }
            }
            tracing::info!(%edge, "edge disconnected");
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

/// Moves complete windows from the collector into the engine, in order.
#[must_use]
pub fn spawn_releaser(
    collector: Arc<Mutex<Collector>>,
    handle: EngineHandle,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut backlog: std::collections::VecDeque<WindowSummary> = std::collections::VecDeque::new();
        loop {
            let stats = {
                let mut c = lock(&collector);
                backlog.extend(c.poll(wall_now()));
                c.stats()
            };
            let m = handle.metrics();
            m.cluster_set("accepted", stats.accepted);
            m.cluster_set("duplicates", stats.duplicates);
            m.cluster_set("gaps", stats.gaps);
            m.cluster_set("stale_epoch", stats.stale_epoch);
            m.cluster_set("late", stats.late);
            m.cluster_set("lost", stats.lost);
            m.cluster_set("deadline_releases", stats.deadline_releases);
            while let Some(w) = backlog.pop_front() {
                match handle.try_ingest(Ingest::Summary(Box::new(w.clone()))) {
                    Ok(()) => {}
                    Err(Rejected::Full) => {
                        backlog.push_front(w);
                        break;
                    }
                    Err(Rejected::Stopped) => return,
                }
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(200)) => {}
                _ = shutdown.changed() => return,
            }
        }
    })
}

/// Requires the cluster token (when one is configured) on the summary stream.
#[derive(Clone)]
pub struct ClusterAuth(pub Option<Arc<Secret>>);

impl tonic::service::Interceptor for ClusterAuth {
    fn call(&mut self, req: Request<()>) -> Result<Request<()>, Status> {
        let Some(expected) = &self.0 else { return Ok(req) };
        let given =
            req.metadata().get("authorization").and_then(|v| v.to_str().ok()).and_then(|h| h.strip_prefix("Bearer "));
        let ok = given
            .is_some_and(|g| bool::from(subtle::ConstantTimeEq::ct_eq(g.as_bytes(), expected.expose().as_bytes())));
        if ok { Ok(req) } else { Err(Status::unauthenticated("missing or invalid cluster token")) }
    }
}

/// Starts the core's summary receiver on `listener`.
///
/// # Errors
/// Fails if the listener has no local address or TLS cannot be configured.
pub fn spawn_receiver(
    listener: tokio::net::TcpListener,
    service: CoreService,
    interceptor: ClusterAuth,
    tls: Option<tonic::transport::ServerTlsConfig>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<(SocketAddr, JoinHandle<()>)> {
    let addr = listener.local_addr()?;
    let mut builder = tonic::transport::Server::builder();
    if let Some(t) = tls {
        builder = builder.tls_config(t)?;
    }
    let router = builder
        .add_service(tonic::service::interceptor::InterceptedService::new(SummariesServer::new(service), interceptor));
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let task = tokio::spawn(async move {
        let signal = async move {
            let _ = shutdown.changed().await;
        };
        if let Err(e) = router.serve_with_incoming_shutdown(incoming, signal).await {
            tracing::error!(error = %e, "summary receiver failed");
        }
    });
    Ok((addr, task))
}
