//! End-to-end behaviour of the engine on a small synthetic system.
//!
//! Topology: `frontend -> cart -> redis (no telemetry)`, `frontend -> catalog`.
//! After ten minutes of normal traffic, `cart` becomes CPU-starved (its own
//! processing time grows by 60 ms) for two minutes. The engine must open an
//! incident, rank `cart` first, and resolve the incident after recovery.

use std::time::Duration;

use etio_core::rng::Rng;
use etio_core::{Interner, Sym};
use etio_engine::{Engine, EngineConfig, Event, IncidentConfig, IncidentStatus};
use etio_pipeline::{Span, SpanKind, SpanStatus};

const MS: i64 = 1_000_000;
const SEC: i64 = 1_000_000_000;
const T0: i64 = 1_700_000_000 * SEC;

struct System {
    rng: Rng,
    frontend: Sym,
    cart: Sym,
    catalog: Sym,
    redis: Sym,
    op: Sym,
    next_id: u64,
}

impl System {
    fn new(interner: &Interner, seed: u64) -> Self {
        Self {
            rng: Rng::seed_from_u64(seed),
            frontend: interner.intern("frontend"),
            cart: interner.intern("cart"),
            catalog: interner.intern("catalog"),
            redis: interner.intern("redis"),
            op: interner.intern("GET /"),
            next_id: 1,
        }
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    #[allow(clippy::too_many_arguments)]
    fn span(&mut self, trace: u128, parent: u64, svc: Sym, kind: SpanKind, start: i64, end: i64, peer: Sym) -> Span {
        Span {
            trace_id: trace,
            span_id: self.id(),
            parent_id: parent,
            service: svc,
            operation: self.op,
            kind,
            start,
            end,
            status: SpanStatus::Unset,
            peer,
        }
    }

    /// One request starting at `t`; `cart_extra` is the fault's added CPU time.
    fn request(&mut self, t: i64, cart_extra: i64) -> Vec<Span> {
        let trace = (u128::from(self.rng.next_u64()) << 64) | u128::from(self.rng.next_u64());
        #[allow(clippy::cast_possible_truncation)]
        let mut lat = |median_ms: f64| (self.rng.lognormal_median_p99(median_ms, 3.0) * 1e6) as i64;
        let fe_pre = lat(2.0);
        let redis = lat(1.0);
        let cart_own = lat(5.0) + cart_extra;
        let catalog = lat(8.0);
        let net = MS / 2;

        let mut out = Vec::with_capacity(8);
        let fe_start = t;
        let cart_client_start = fe_start + fe_pre;
        let cart_server_start = cart_client_start + net;
        let redis_start = cart_server_start + cart_own / 2;
        let redis_end = redis_start + redis;
        let cart_server_end = redis_end + cart_own / 2;
        let cart_client_end = cart_server_end + net;
        let cat_client_start = cart_client_end;
        let cat_server_start = cat_client_start + net;
        let cat_server_end = cat_server_start + catalog;
        let cat_client_end = cat_server_end + net;
        let fe_end = cat_client_end + MS;

        let root = self.span(trace, 0, self.frontend, SpanKind::Server, fe_start, fe_end, Sym::EMPTY);
        let cc = self.span(
            trace,
            root.span_id,
            self.frontend,
            SpanKind::Client,
            cart_client_start,
            cart_client_end,
            Sym::EMPTY,
        );
        let cs =
            self.span(trace, cc.span_id, self.cart, SpanKind::Server, cart_server_start, cart_server_end, Sym::EMPTY);
        let rc = self.span(trace, cs.span_id, self.cart, SpanKind::Client, redis_start, redis_end, self.redis);
        let kc = self.span(
            trace,
            root.span_id,
            self.frontend,
            SpanKind::Client,
            cat_client_start,
            cat_client_end,
            Sym::EMPTY,
        );
        let ks =
            self.span(trace, kc.span_id, self.catalog, SpanKind::Server, cat_server_start, cat_server_end, Sym::EMPTY);
        out.extend([root, cc, cs, rc, kc, ks]);
        out
    }
}

fn config() -> EngineConfig {
    EngineConfig {
        resolution: Duration::from_secs(1),
        lateness: Duration::from_secs(3),
        trace_timeout: Duration::from_secs(1),
        retention: Duration::from_secs(40 * 60),
        incident: IncidentConfig {
            reference: Duration::from_secs(8 * 60),
            rca_delay: Duration::from_secs(15),
            rca_interval: Duration::from_secs(30),
            resolve_after: Duration::from_secs(60),
            ..IncidentConfig::default()
        },
        ..EngineConfig::default()
    }
}

/// Runs the scenario and returns all events with the time they were emitted.
fn run(seed: u64) -> (Engine, Vec<(i64, Event)>) {
    let mut engine = Engine::new(config()).expect("valid config");
    let interner = engine.interner().clone();
    let mut sys = System::new(&interner, seed);
    let mut events = Vec::new();
    let fault = (T0 + 600 * SEC, T0 + 720 * SEC);
    let tick = 100 * MS;
    let mut t = T0;
    while t < T0 + 900 * SEC {
        // 20 requests per second, spread over the tick.
        let mut batch = Vec::new();
        for k in 0..2 {
            let start = t + k * 50 * MS;
            let extra = if (fault.0..fault.1).contains(&start) { 60 * MS } else { 0 };
            batch.extend(sys.request(start, extra));
        }
        // Spans are exported when they end; deliver them a tick later.
        engine.ingest_spans(batch);
        t += tick;
        for e in engine.advance(t) {
            events.push((t, e));
        }
    }
    for e in engine.flush() {
        events.push((t, e));
    }
    (engine, events)
}

#[test]
fn detects_localises_and_resolves_a_cpu_fault() {
    let (engine, events) = run(1);
    let opened: Vec<_> = events.iter().filter(|(_, e)| matches!(e, Event::IncidentOpened { .. })).collect();
    assert_eq!(opened.len(), 1, "exactly one incident: {:?}", engine.incidents().map(|i| &i.id).collect::<Vec<_>>());
    let (opened_at, _) = opened[0];
    let fault_start = T0 + 600 * SEC;
    assert!(*opened_at > fault_start, "no incident before the fault");
    assert!(*opened_at - fault_start < 30 * SEC, "detected within 30 s, took {} s", (*opened_at - fault_start) / SEC);

    let first = events
        .iter()
        .find_map(|(_, e)| match e {
            Event::IncidentAnalyzed { incident } => Some(incident),
            _ => None,
        })
        .expect("analysed");
    let rca = first.rca.as_ref().expect("analysis attached");
    assert_eq!(rca.ranking[0].service, "cart", "ranking: {:?}", rca.services());
    assert!(!rca.ranking[0].reasons.is_empty());

    let inc = engine.incidents().next().expect("incident kept");
    assert_eq!(inc.status, IncidentStatus::Resolved);
    let resolved_at = inc.resolved_at.expect("resolved");
    assert!(resolved_at > T0 + 720 * SEC && resolved_at < T0 + 900 * SEC);
    assert!(inc.analyses >= 2);
    let stats = engine.stats();
    assert_eq!(stats.aggregator.late, 0, "no data arrived after its window closed");
    assert!(stats.traces > 17_000);
}

#[test]
fn engine_is_deterministic() {
    let (a, ea) = run(7);
    let (b, eb) = run(7);
    assert_eq!(ea, eb);
    assert_eq!(a.stats(), b.stats());
}

#[test]
fn quiet_system_opens_no_incident() {
    let mut engine = Engine::new(config()).expect("valid config");
    let interner = engine.interner().clone();
    let mut sys = System::new(&interner, 3);
    let mut t = T0;
    let mut events = Vec::new();
    while t < T0 + 900 * SEC {
        let batch: Vec<Span> = (0..2).flat_map(|k| sys.request(t + k * 50 * MS, 0)).collect();
        engine.ingest_spans(batch);
        t += 100 * MS;
        events.extend(engine.advance(t));
    }
    assert!(
        events.is_empty(),
        "false alarms: {:?}",
        events.iter().map(|e| format!("{e:?}").chars().take(120).collect::<String>()).collect::<Vec<_>>()
    );
}

#[test]
fn restored_engine_keeps_its_baselines() {
    // Learn for 12 minutes, snapshot mid-stream (with traces and windows in
    // flight), restore into a fresh engine, and continue with a fault at 800 s.
    let mut a = Engine::new(config()).expect("valid config");
    let mut sys = System::new(&a.interner().clone(), 11);
    let mut t = T0;
    while t < T0 + 720 * SEC {
        let batch: Vec<Span> = (0..2).flat_map(|k| sys.request(t + k * 50 * MS, 0)).collect();
        a.ingest_spans(batch);
        t += 100 * MS;
        a.advance(t);
    }
    let bytes = postcard::to_stdvec(&a.snapshot()).expect("serialisable");
    let snap: etio_engine::EngineSnapshot = postcard::from_bytes(&bytes).expect("deserialisable");
    let mut b = Engine::restore(config(), snap).expect("compatible");
    assert_eq!(b.store().len(), a.store().len());

    let mut sys = System::new(&b.interner().clone(), 12);
    let mut events = Vec::new();
    while t < T0 + 1_000 * SEC {
        let extra = |at: i64| if at > T0 + 800 * SEC && at < T0 + 900 * SEC { 60 * MS } else { 0 };
        let batch: Vec<Span> = (0..2).flat_map(|k| sys.request(t + k * 50 * MS, extra(t + k * 50 * MS))).collect();
        b.ingest_spans(batch);
        t += 100 * MS;
        events.extend(b.advance(t).into_iter().map(|e| (t, e)));
    }
    let opened: Vec<i64> =
        events.iter().filter(|(_, e)| matches!(e, Event::IncidentOpened { .. })).map(|(at, _)| *at).collect();
    assert_eq!(opened.len(), 1, "no false alarm around the restore, one real incident: {opened:?}");
    assert!(opened[0] > T0 + 800 * SEC && opened[0] < T0 + 830 * SEC, "detected without re-calibration");
    let analysed = events.iter().find_map(|(_, e)| match e {
        Event::IncidentAnalyzed { incident } => incident.rca.as_ref(),
        _ => None,
    });
    assert_eq!(analysed.expect("analysed").ranking[0].service, "cart");

    let mut wrong = config();
    wrong.resolution = Duration::from_secs(2);
    wrong.lateness = Duration::from_secs(6);
    assert!(matches!(Engine::restore(wrong, b.snapshot()), Err(etio_engine::RestoreError::Resolution { .. })));
}

/// Counters exported at the same period as the window width, with jitter,
/// must yield a steady rate: no false drops to zero in windows that happen to
/// receive no export, no doubled values in windows that receive two. Two
/// streams (e.g. two network interfaces) must add up.
#[test]
fn counter_rates_do_not_depend_on_export_alignment() {
    use etio_engine::{MetricKind, MetricPoint};

    let mut engine =
        Engine::new(EngineConfig { resolution: Duration::from_secs(5), lateness: Duration::from_secs(10), ..config() })
            .unwrap();
    let interner = engine.interner().clone();
    let (svc, name) = (interner.intern("cart"), interner.intern("system.network.packets"));
    let mut rng = Rng::seed_from_u64(9);
    // Two streams at 100 and 50 packets per second, exported every 5 s ± 1 s.
    let mut totals = [0.0f64; 2];
    let mut t = T0;
    for _ in 0..200 {
        let step = 5 * SEC + i64::try_from(rng.below(2_000)).unwrap() * MS - SEC;
        let secs = f64::from(u32::try_from(step / MS).unwrap()) / 1000.0;
        t += step;
        let mut points = Vec::new();
        for (stream, rate) in [(1u64, 100.0), (2, 50.0)] {
            let i = usize::try_from(stream - 1).unwrap();
            totals[i] += rate * secs;
            points.push(MetricPoint {
                service: svc,
                name,
                stream,
                ts: t,
                value: totals[i],
                kind: MetricKind::Cumulative { start: T0 },
            });
        }
        engine.ingest_metrics(&points);
        engine.advance(t);
    }
    let id = engine.store().find("cart", "system.network.packets").unwrap();
    let last = engine.store().last_window().unwrap();
    let values: Vec<f64> = engine.store().read(id, last - 150, last).into_iter().filter(|v| v.is_finite()).collect();
    assert!(values.len() > 100, "most windows have a value ({})", values.len());
    for v in values {
        assert!((v - 150.0).abs() < 1e-6, "rate {v} instead of 150/s");
    }
}
