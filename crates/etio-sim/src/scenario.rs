//! Scenarios with ground truth, and running them through the engine.

use etio_engine::{Engine, EngineConfig, Event, LogEntry};
use serde::{Deserialize, Serialize};

use crate::sim::{Fault, FaultKind, Simulation, Workload};
use crate::telemetry::to_engine;
use crate::topology::{Kind, Topology};

/// A reproducible experiment: a system, a workload and faults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    /// Human-readable name.
    pub name: String,
    /// The system.
    pub topology: Topology,
    /// Traffic.
    pub workload: Workload,
    /// Injected faults (the ground truth).
    pub faults: Vec<Fault>,
    /// Random seed.
    pub seed: u64,
    /// Length, seconds.
    pub duration_s: f64,
}

/// What happened when a scenario ran through the engine.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    /// Scenario name.
    pub scenario: String,
    /// Number of services.
    pub services: usize,
    /// Injected fault (first one).
    pub fault: Option<Fault>,
    /// Incidents opened before the fault (false alarms).
    pub false_incidents: usize,
    /// Seconds from fault start to the incident opening, if detected.
    pub detection_delay_s: Option<f64>,
    /// 1-based rank of the faulty service in the first analysis of the incident.
    pub first_rank: Option<usize>,
    /// Rank in the last analysis of the incident.
    pub last_rank: Option<usize>,
    /// Top candidate of the first analysis.
    pub first_top: Option<String>,
    /// Spans processed.
    pub spans: u64,
}

impl Scenario {
    /// The engine configuration used for simulated scenarios: one-second
    /// windows so that short scenarios have enough history.
    #[must_use]
    pub fn engine_config() -> EngineConfig {
        let mut cfg = EngineConfig {
            resolution: std::time::Duration::from_secs(1),
            lateness: std::time::Duration::from_secs(4),
            trace_timeout: std::time::Duration::from_secs(2),
            retention: std::time::Duration::from_secs(60 * 60),
            ..EngineConfig::default()
        };
        cfg.incident.reference = std::time::Duration::from_secs(10 * 60);
        cfg.incident.rca_delay = std::time::Duration::from_secs(20);
        cfg.incident.resolve_after = std::time::Duration::from_secs(90);
        cfg
    }

    /// The fault kinds of the scale benchmark, for application services.
    pub const KINDS: [&'static str; 6] = ["cpu", "delay", "loss", "errors", "memory_leak", "crash"];

    /// A random scenario: a layered topology of `services` services and one
    /// fault of kind `KINDS[kind % 6]` on a random service, starting after
    /// `warmup_s`. Datastores only receive the kinds that apply to them.
    #[must_use]
    pub fn random_with_kind(services: usize, seed: u64, kind: usize, warmup_s: f64, fault_s: f64) -> Self {
        let mut s = Self::random(services, seed, warmup_s, fault_s);
        let target = s.faults[0].target.clone();
        let is_store = s.topology.services.iter().any(|x| x.name == target && x.kind == Kind::Datastore);
        s.faults[0].kind = match (Self::KINDS[kind % Self::KINDS.len()], is_store) {
            ("cpu", true) => FaultKind::Cpu { factor: 8.0 },
            ("cpu", false) => FaultKind::Cpu { factor: 6.0 },
            ("delay", true) => FaultKind::Delay { ms: 40.0 },
            ("delay", false) | ("loss", true) => FaultKind::Delay { ms: 60.0 },
            ("loss", false) => FaultKind::Loss { rate: 0.15 },
            ("errors", _) => FaultKind::Errors { rate: 0.25 },
            ("memory_leak", false) => FaultKind::MemoryLeak { mb_per_s: 4.0 },
            ("crash", false) => FaultKind::Crash,
            (_, true) => FaultKind::Cpu { factor: 8.0 },
            _ => FaultKind::Crash,
        };
        s.name = format!("layered-{services}-{seed}-{}", s.faults[0].kind.name());
        s
    }

    /// A random scenario: a layered topology of `services` services and one
    /// random fault on a random service, starting after `warmup_s`.
    #[must_use]
    pub fn random(services: usize, seed: u64, warmup_s: f64, fault_s: f64) -> Self {
        let topology = Topology::layered(services, seed);
        let workload = Workload { rate: 25.0, ..Workload::default() };
        let mut rng = etio_core::rng::Rng::seed_from_u64(seed).fork("scenario");
        // Faults target services that actually receive traffic (at least
        // 0.5 calls/s in a pilot run): a fault in a service nobody calls has
        // no symptom, and counting it as a miss would measure the topology
        // generator, not the engine. Entry services (the gateways) are excluded.
        let traffic = Self::pilot_traffic(&topology, &workload, seed);
        let busy: Vec<usize> = (0..topology.services.len())
            .filter(|&i| !topology.entries.iter().any(|e| e.service == i) && traffic[i] >= 0.5)
            .collect();
        let candidates: Vec<usize> = if busy.is_empty() {
            (0..topology.services.len()).filter(|&i| !topology.entries.iter().any(|e| e.service == i)).collect()
        } else {
            busy
        };
        let target = candidates[rng.index(candidates.len())];
        let is_store = topology.services[target].kind == Kind::Datastore;
        let kinds: Vec<FaultKind> = if is_store {
            vec![FaultKind::Cpu { factor: 8.0 }, FaultKind::Delay { ms: 40.0 }, FaultKind::Errors { rate: 0.3 }]
        } else {
            vec![
                FaultKind::Cpu { factor: 6.0 },
                FaultKind::Delay { ms: 60.0 },
                FaultKind::Loss { rate: 0.15 },
                FaultKind::Errors { rate: 0.25 },
                FaultKind::MemoryLeak { mb_per_s: 4.0 },
                FaultKind::Crash,
            ]
        };
        let kind = kinds[rng.index(kinds.len())].clone();
        let fault =
            Fault { target: topology.services[target].name.clone(), kind, start_s: warmup_s, duration_s: fault_s };
        Self {
            name: format!("layered-{services}-{seed}"),
            topology,
            workload,
            faults: vec![fault],
            seed,
            duration_s: warmup_s + fault_s + 60.0,
        }
    }

    /// Calls per second received by each service during a one-minute pilot run.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn pilot_traffic(topology: &Topology, workload: &Workload, seed: u64) -> Vec<f64> {
        let mut counts = vec![0usize; topology.services.len()];
        let Ok(mut sim) = Simulation::new(topology.clone(), workload.clone(), Vec::new(), seed ^ 0x5eed) else {
            return vec![0.0; topology.services.len()];
        };
        for _ in 0..60 {
            for span in sim.step(1.0).spans {
                if let Some(peer) = span.peer.as_deref().or(span.db.as_deref())
                    && let Some(i) = topology.service_index(peer)
                {
                    counts[i] += 1;
                }
            }
        }
        counts.into_iter().map(|c| c as f64 / 60.0).collect()
    }

    /// Runs the scenario through a fresh engine, in accelerated time.
    ///
    /// # Errors
    /// Fails if the scenario is invalid.
    pub fn run(&self, cfg: EngineConfig) -> Result<Outcome, String> {
        let mut engine = Engine::new(cfg).map_err(|e| e.to_string())?;
        let mut sim = Simulation::new(self.topology.clone(), self.workload.clone(), self.faults.clone(), self.seed)?;
        let interner = engine.interner().clone();
        let t0: i64 = 1_700_000_000_000_000_000;
        let tick = 0.5;
        let mut events: Vec<(f64, Event)> = Vec::new();
        while sim.time() < self.duration_s {
            let batch = sim.step(tick);
            let rec = to_engine(&batch, &self.topology, &interner, t0);
            engine.ingest_spans(rec.spans);
            engine.ingest_metrics(&rec.metrics);
            let logs: Vec<LogEntry<'_>> = rec
                .logs
                .iter()
                .map(|(ts, svc, body, sev)| LogEntry { ts: *ts, service: *svc, body, severity: *sev })
                .collect();
            engine.ingest_logs(&logs);
            #[allow(clippy::cast_possible_truncation)]
            let now = t0 + (sim.time() * 1e9) as i64;
            for e in engine.advance(now) {
                events.push((sim.time(), e));
            }
        }
        for e in engine.flush() {
            events.push((sim.time(), e));
        }
        let fault = self.faults.first().cloned();
        let fault_start = fault.as_ref().map_or(f64::INFINITY, |f| f.start_s);
        let false_incidents =
            events.iter().filter(|(t, e)| matches!(e, Event::IncidentOpened { .. }) && *t < fault_start).count();
        let opened = events.iter().find(|(t, e)| matches!(e, Event::IncidentOpened { .. }) && *t >= fault_start);
        let incident_id = opened.and_then(|(_, e)| match e {
            Event::IncidentOpened { incident } => Some(incident.id.clone()),
            _ => None,
        });
        let analyses: Vec<&etio_analysis::RcaResult> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::IncidentAnalyzed { incident } | Event::IncidentResolved { incident }
                    if Some(&incident.id) == incident_id.as_ref() =>
                {
                    incident.rca.as_ref()
                }
                _ => None,
            })
            .collect();
        let target = fault.as_ref().map(|f| f.target.as_str()).unwrap_or_default();
        Ok(Outcome {
            scenario: self.name.clone(),
            services: self.topology.services.len(),
            detection_delay_s: opened.map(|(t, _)| t - fault_start),
            first_rank: analyses.first().and_then(|r| r.rank_of(target)),
            last_rank: analyses.last().and_then(|r| r.rank_of(target)),
            first_top: analyses.first().and_then(|r| r.ranking.first()).map(|r| r.service.clone()),
            fault,
            false_incidents,
            spans: engine.stats().spans,
        })
    }
}
