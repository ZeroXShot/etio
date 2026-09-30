//! The engine actor.
//!
//! The engine is a single-threaded state machine, so it lives on a dedicated
//! OS thread that owns it exclusively. Everything else talks to it through
//! two channels:
//!
//! * a **bounded ingest queue** carrying decoded telemetry. When it is full,
//!   receivers answer with the OTLP backpressure signals (`RESOURCE_EXHAUSTED`
//!   over gRPC, `429 Too Many Requests` with `Retry-After` over HTTP), so
//!   well-behaved exporters slow down instead of the server running out of
//!   memory;
//! * a small **control channel**, always served first, carrying queries and
//!   commands (API reads, snapshots, shutdown). API reads therefore stay
//!   responsive while ingestion is saturated.
//!
//! The actor also drives the engine clock: every `tick` it advances the
//! engine to the wall clock, publishes the resulting events, and refreshes
//! the engine gauges. What advancing means depends on the [`Mode`].

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select};
use etio_core::Interner;
use etio_engine::{Engine, Event, MetricPoint, WindowSummary};
use etio_otlp::decode::logs::LogBatch;
use etio_pipeline::Span;
use tokio::sync::{broadcast, oneshot};

use crate::metrics::Metrics;

/// Decoded telemetry for the engine.
pub enum Ingest {
    /// Spans.
    Spans(Vec<Span>),
    /// Metric points.
    Metrics(Vec<MetricPoint>),
    /// Log records.
    Logs(LogBatch),
    /// A window summary from an edge node (distributed mode).
    Summary(Box<WindowSummary>),
}

type Job = Box<dyn FnOnce(&mut Engine) + Send>;

enum Control {
    Run(Job),
    Advance(i64),
    Shutdown,
}

/// How the engine clock advances.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Clock {
    /// Follow the wall clock (production).
    Wall,
    /// Follow the newest telemetry timestamp (replays, faster-than-real-time
    /// simulations). A client with a clock far in the future moves it for
    /// everyone, so production deployments use [`Clock::Wall`].
    Event,
    /// Advance only on explicit [`EngineHandle::advance`] calls (tests).
    Manual,
}

/// The role of the engine in a deployment.
#[derive(Clone, Default)]
pub enum Mode {
    /// Close windows and analyse them locally.
    #[default]
    Standalone,
    /// Close windows and queue their summaries for the cores; no detection.
    Edge(Arc<crate::cluster::EdgeQueue>),
    /// Apply summaries released by the collector; the clock follows them.
    /// A core must not close windows itself: sealing empty local windows
    /// would make every window the edges send look stale.
    Core,
}

/// Why telemetry was not accepted.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Rejected {
    /// The ingest queue is full; the client should retry later.
    #[error("ingest queue is full")]
    Full,
    /// The engine has stopped.
    #[error("engine is shutting down")]
    Stopped,
}

/// A cloneable handle to the engine actor.
#[derive(Clone)]
pub struct EngineHandle {
    ingest: Sender<Ingest>,
    control: Sender<Control>,
    interner: Arc<Interner>,
    events: broadcast::Sender<Arc<Event>>,
    metrics: Arc<Metrics>,
}

/// Nanoseconds since the epoch, from the wall clock.
#[must_use]
pub fn wall_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

impl EngineHandle {
    /// Moves `engine` onto its own thread and returns a handle to it and the
    /// thread's join handle (which yields the engine back after shutdown).
    ///
    /// # Panics
    /// Panics if the OS refuses to spawn a thread.
    #[must_use]
    pub fn spawn(
        engine: Engine,
        queue: usize,
        tick: Duration,
        clock: Clock,
        metrics: Arc<Metrics>,
    ) -> (Self, JoinHandle<Engine>) {
        Self::spawn_with_mode(engine, queue, tick, clock, metrics, Mode::Standalone)
    }

    /// Like [`EngineHandle::spawn`], in a given [`Mode`].
    ///
    /// # Panics
    /// Panics if the OS refuses to spawn a thread.
    #[must_use]
    pub fn spawn_with_mode(
        engine: Engine,
        queue: usize,
        tick: Duration,
        clock: Clock,
        metrics: Arc<Metrics>,
        mode: Mode,
    ) -> (Self, JoinHandle<Engine>) {
        let (ingest_tx, ingest_rx) = bounded(queue.max(1));
        let (control_tx, control_rx) = bounded(64);
        let (events, _) = broadcast::channel(1024);
        let handle = Self {
            ingest: ingest_tx,
            control: control_tx,
            interner: engine.interner().clone(),
            events: events.clone(),
            metrics: metrics.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("etio-engine".into())
            .spawn(move || run(engine, &ingest_rx, &control_rx, &events, &metrics, tick, clock, &mode))
            .unwrap_or_else(|e| panic!("cannot spawn the engine thread: {e}"));
        (handle, thread)
    }

    /// The interner decoders must use.
    #[must_use]
    pub fn interner(&self) -> &Arc<Interner> {
        &self.interner
    }

    /// Server metrics.
    #[must_use]
    pub fn metrics(&self) -> &Arc<Metrics> {
        &self.metrics
    }

    /// Queues telemetry without blocking.
    ///
    /// # Errors
    /// Returns [`Rejected::Full`] when the queue is full.
    pub fn try_ingest(&self, batch: Ingest) -> Result<(), Rejected> {
        let r = self.ingest.try_send(batch).map_err(|e| match e {
            TrySendError::Full(_) => Rejected::Full,
            TrySendError::Disconnected(_) => Rejected::Stopped,
        });
        #[allow(clippy::cast_possible_wrap)]
        self.metrics.queue_depth.set(self.ingest.len() as i64);
        r
    }

    /// Runs `f` on the engine thread and returns its result.
    ///
    /// # Errors
    /// Returns [`Rejected::Stopped`] if the engine has stopped.
    pub async fn with_engine<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Engine) -> R + Send + 'static,
    ) -> Result<R, Rejected> {
        let (tx, rx) = oneshot::channel();
        let job: Job = Box::new(move |engine| {
            let _ = tx.send(f(engine));
        });
        // The control channel is small and served first; waiting on it off the
        // async runtime keeps executor threads free.
        let control = self.control.clone();
        tokio::task::spawn_blocking(move || control.send(Control::Run(job)))
            .await
            .map_err(|_| Rejected::Stopped)?
            .map_err(|_| Rejected::Stopped)?;
        rx.await.map_err(|_| Rejected::Stopped)
    }

    /// Advances a [`Clock::Manual`] engine to `now` (ns since the epoch).
    ///
    /// # Errors
    /// Returns [`Rejected::Stopped`] if the engine has stopped.
    pub fn advance(&self, now: i64) -> Result<(), Rejected> {
        self.control.send(Control::Advance(now)).map_err(|_| Rejected::Stopped)
    }

    /// Subscribes to engine events.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.events.subscribe()
    }

    /// Asks the actor to stop after the work already queued.
    pub fn shutdown(&self) {
        let _ = self.control.send(Control::Shutdown);
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    mut engine: Engine,
    ingest: &Receiver<Ingest>,
    control: &Receiver<Control>,
    events: &broadcast::Sender<Arc<Event>>,
    metrics: &Metrics,
    tick: Duration,
    clock: Clock,
    mode: &Mode,
) -> Engine {
    let mut next_tick = Instant::now() + tick;
    let publish = |engine: &mut Engine, evs: Vec<Event>| {
        for e in evs {
            let _ = events.send(Arc::new(e));
        }
        publish_stats(engine, metrics);
    };
    let advance = |engine: &mut Engine, now: i64| {
        let started = Instant::now();
        match mode {
            Mode::Standalone => {
                let evs = engine.advance(now);
                publish(engine, evs);
            }
            Mode::Edge(queue) => {
                let st = queue.push(engine.advance_edge(now));
                metrics.cluster_set("enqueued", st.enqueued);
                metrics.cluster_set("dropped", st.dropped);
                metrics.cluster_set("retained", queue.len() as u64);
                publish_stats(engine, metrics);
            }
            Mode::Core => publish_stats(engine, metrics),
        }
        metrics.tick_seconds.observe(started.elapsed().as_secs_f64());
    };
    let apply = |engine: &mut Engine, batch: Ingest| {
        if let Ingest::Summary(s) = &batch {
            let evs = engine.apply_summary(s);
            publish(engine, evs);
        } else {
            apply_telemetry(engine, batch);
        }
    };
    loop {
        // Control messages always go first.
        while let Ok(msg) = control.try_recv() {
            match msg {
                Control::Run(job) => job(&mut engine),
                Control::Advance(now) => advance(&mut engine, now),
                Control::Shutdown => return drain(engine, ingest),
            }
        }
        let timeout = next_tick.saturating_duration_since(Instant::now());
        select! {
            recv(control) -> msg => match msg {
                Ok(Control::Run(job)) => job(&mut engine),
                Ok(Control::Advance(now)) => advance(&mut engine, now),
                Ok(Control::Shutdown) | Err(_) => return drain(engine, ingest),
            },
            recv(ingest) -> msg => match msg {
                Ok(batch) => apply(&mut engine, batch),
                Err(_) => return engine,
            },
            default(timeout) => {
                match clock {
                    Clock::Wall => advance(&mut engine, wall_now()),
                    Clock::Event => {
                        if let Some(t) = engine.latest_event() {
                            advance(&mut engine, t);
                        }
                    }
                    Clock::Manual => {}
                }
                next_tick += tick;
                if next_tick < Instant::now() {
                    // Fell behind (e.g. a long analysis): do not burst to catch up.
                    next_tick = Instant::now() + tick;
                }
            }
        }
    }
}

/// Applies what is still queued (events are no longer published).
fn drain(mut engine: Engine, ingest: &Receiver<Ingest>) -> Engine {
    while let Ok(batch) = ingest.try_recv() {
        if let Ingest::Summary(s) = &batch {
            engine.apply_summary(s);
        } else {
            apply_telemetry(&mut engine, batch);
        }
    }
    engine
}

fn apply_telemetry(engine: &mut Engine, batch: Ingest) {
    match batch {
        Ingest::Spans(spans) => engine.ingest_spans(spans),
        Ingest::Metrics(points) => engine.ingest_metrics(&points),
        Ingest::Logs(logs) => {
            let entries: Vec<_> = logs.entries().collect();
            engine.ingest_logs(&entries);
        }
        Ingest::Summary(_) => {}
    }
}

#[allow(clippy::cast_possible_wrap)]
fn publish_stats(engine: &Engine, m: &Metrics) {
    let s = engine.stats();
    m.series.set(engine.store().len() as i64);
    m.buffered_spans.set(engine.buffered_spans() as i64);
    m.windows.set(s.windows);
    m.late.set(s.aggregator.late);
    m.traces.set(s.traces);
    m.analyses.set(s.analyses);
    m.incidents_open.set(i64::from(engine.incidents().any(|i| i.status == etio_engine::IncidentStatus::Open)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use etio_engine::EngineConfig;

    #[tokio::test]
    async fn queries_run_on_the_engine_thread_and_shutdown_returns_the_engine() {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let (h, thread) =
            EngineHandle::spawn(engine, 4, Duration::from_millis(50), Clock::Manual, Arc::new(Metrics::new()));
        h.try_ingest(Ingest::Spans(Vec::new())).unwrap();
        let n = h.with_engine(|e| e.store().len()).await.unwrap();
        assert_eq!(n, 0);
        h.advance(1_000_000_000).unwrap();
        let now = h.with_engine(|e| e.now()).await.unwrap();
        assert_eq!(now, 1_000_000_000);
        h.shutdown();
        let engine = tokio::task::spawn_blocking(move || thread.join().unwrap()).await.unwrap();
        assert_eq!(engine.now(), 1_000_000_000);
        assert_eq!(h.with_engine(|e| e.now()).await, Err(Rejected::Stopped));
    }

    #[tokio::test]
    async fn full_queue_is_reported() {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let (h, _thread) =
            EngineHandle::spawn(engine, 1, Duration::from_secs(3600), Clock::Manual, Arc::new(Metrics::new()));
        // Block the engine thread so the queue cannot drain.
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let blocker = h.clone();
        tokio::spawn(async move {
            blocker
                .with_engine(move |_| {
                    let _ = release_rx.recv();
                })
                .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut full = false;
        for _ in 0..4 {
            if h.try_ingest(Ingest::Spans(Vec::new())) == Err(Rejected::Full) {
                full = true;
            }
        }
        assert!(full);
        release_tx.send(()).unwrap();
    }
}
