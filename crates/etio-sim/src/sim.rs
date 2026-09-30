//! The simulation: requests flowing through a topology under faults.
//!
//! Each request is expanded depth-first into its span tree. Interactions
//! between requests are captured with a *mean-field* queueing model rather
//! than explicit queues: every service tracks the processing work it has
//! received recently (an exponentially weighted busy-time rate); its
//! utilisation `ρ = work rate / capacity` sets the queueing delay through
//! Sakasegawa's M/M/c approximation, and when `ρ > 1` a backlog accumulates
//! and drains at the service's capacity. A CPU fault that doubles processing
//! time therefore does not double latency: it pushes `ρ` towards 1, waits
//! explode non-linearly, callers hit their timeouts and errors propagate
//! upstream, as in real systems. The model is O(spans) and deterministic.

use etio_core::rng::Rng;
use serde::{Deserialize, Serialize};

use crate::topology::{Kind, Topology};

const MS: f64 = 1e-3;

/// What goes wrong.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FaultKind {
    /// Processing takes `factor` times longer (CPU starvation or contention).
    Cpu {
        /// Slowdown factor (> 1).
        factor: f64,
    },
    /// Network delay added to every call into the target.
    Delay {
        /// Added one-way delay, milliseconds.
        ms: f64,
    },
    /// A fraction of calls into the target lose packets and wait for retransmission.
    Loss {
        /// Fraction of affected calls.
        rate: f64,
    },
    /// The target fails requests (a bug, a bad deployment).
    Errors {
        /// Failure probability.
        rate: f64,
    },
    /// Memory grows until the process is killed and restarts.
    MemoryLeak {
        /// Growth, MiB per second.
        mb_per_s: f64,
    },
    /// The target is down: connections are refused and it emits nothing.
    Crash,
}

impl FaultKind {
    /// Short name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Cpu { .. } => "cpu",
            Self::Delay { .. } => "delay",
            Self::Loss { .. } => "loss",
            Self::Errors { .. } => "errors",
            Self::MemoryLeak { .. } => "memory_leak",
            Self::Crash => "crash",
        }
    }
}

/// A fault injected into one service for a period.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fault {
    /// Target service name.
    pub target: String,
    /// What happens.
    pub kind: FaultKind,
    /// Start, seconds after the beginning of the scenario.
    pub start_s: f64,
    /// Duration, seconds.
    pub duration_s: f64,
}

impl Fault {
    fn active(&self, t: f64) -> bool {
        t >= self.start_s && t < self.start_s + self.duration_s
    }
}

/// A simulated span.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimSpan {
    /// Trace identifier.
    pub trace_id: u128,
    /// Span identifier.
    pub span_id: u64,
    /// Parent span identifier (0 for roots).
    pub parent_id: u64,
    /// Emitting service index.
    pub service: usize,
    /// Operation name.
    pub name: String,
    /// `true` for server spans, `false` for client spans.
    pub server: bool,
    /// Start, seconds since the scenario start.
    pub start: f64,
    /// End, seconds since the scenario start.
    pub end: f64,
    /// Failed.
    pub error: bool,
    /// Datastore name for client spans to uninstrumented datastores.
    pub db: Option<String>,
    /// Called service for client spans (`peer.service`).
    pub peer: Option<String>,
}

/// A simulated metric sample.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SimMetric {
    /// Service index.
    pub service: usize,
    /// Metric name.
    pub name: &'static str,
    /// Time, seconds since the scenario start.
    pub t: f64,
    /// Value.
    pub value: f64,
}

/// A simulated log line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimLog {
    /// Service index.
    pub service: usize,
    /// Time, seconds since the scenario start.
    pub t: f64,
    /// Error severity.
    pub error: bool,
    /// Text.
    pub body: String,
}

/// Telemetry produced during one tick.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Batch {
    /// Spans that ended during the tick.
    pub spans: Vec<SimSpan>,
    /// Metric samples.
    pub metrics: Vec<SimMetric>,
    /// Log lines.
    pub logs: Vec<SimLog>,
}

#[derive(Clone, Debug)]
struct ServiceState {
    /// EWMA of processing seconds received per second.
    work_rate: f64,
    /// Queued work in excess of capacity, seconds of processing.
    backlog: f64,
    /// Work received during the current tick.
    tick_work: f64,
    memory_mb: f64,
    down_until: f64,
    requests: u64,
    errors: u64,
}

/// Parameters of a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workload {
    /// Mean requests per second at the entry points.
    pub rate: f64,
    /// Amplitude of a slow sinusoidal modulation of the rate (0 = flat).
    pub modulation: f64,
    /// Period of the modulation, seconds.
    pub period_s: f64,
    /// Fraction of successful requests that log a line.
    pub log_sampling: f64,
    /// Interval between metric samples, seconds.
    pub metric_interval_s: f64,
}

impl Default for Workload {
    fn default() -> Self {
        Self { rate: 40.0, modulation: 0.2, period_s: 1_800.0, log_sampling: 0.05, metric_interval_s: 5.0 }
    }
}

/// The simulator.
#[derive(Debug)]
pub struct Simulation {
    topology: Topology,
    workload: Workload,
    faults: Vec<Fault>,
    state: Vec<ServiceState>,
    rng: Rng,
    t: f64,
    next_metrics: f64,
    next_id: u64,
    total_weight: f64,
}

const SMOOTHING_S: f64 = 10.0;
const WARMUP_S: f64 = 120.0;
const MEMORY_LIMIT_FACTOR: f64 = 2.5;
const RESTART_S: f64 = 20.0;

/// Sakasegawa's approximation of the mean M/M/c queueing delay.
fn queue_wait(rho: f64, servers: f64, mean_service: f64) -> f64 {
    let rho = rho.clamp(0.0, 0.995);
    let c = servers.max(1.0);
    mean_service * rho.powf((2.0 * (c + 1.0)).sqrt() - 1.0) / (c * (1.0 - rho))
}

impl Simulation {
    /// Creates a simulation.
    ///
    /// # Errors
    /// Fails if the topology is invalid or a fault targets an unknown service.
    pub fn new(topology: Topology, workload: Workload, faults: Vec<Fault>, seed: u64) -> Result<Self, String> {
        topology.validate().map_err(|e| e.to_string())?;
        for f in &faults {
            if topology.service_index(&f.target).is_none() {
                return Err(format!("fault targets unknown service `{}`", f.target));
            }
        }
        let state = topology
            .services
            .iter()
            .map(|s| ServiceState {
                work_rate: 0.0,
                backlog: 0.0,
                tick_work: 0.0,
                memory_mb: s.memory_mb,
                down_until: -1.0,
                requests: 0,
                errors: 0,
            })
            .collect();
        let total_weight = topology.entries.iter().map(|e| e.weight).sum();
        let mut sim = Self {
            topology,
            workload,
            faults: Vec::new(),
            state,
            rng: Rng::seed_from_u64(seed).fork("simulation"),
            t: 0.0,
            next_metrics: 0.0,
            next_id: 1,
            total_weight,
        };
        // A monitored system is already running when monitoring starts: run
        // a silent warm-up so load estimates, queues and memory start in
        // steady state instead of ramping up from zero.
        for _ in 0..(WARMUP_S / 0.5) as usize {
            let _ = sim.step(0.5);
        }
        sim.t = 0.0;
        sim.next_metrics = 0.0;
        sim.faults = faults;
        Ok(sim)
    }

    /// Seconds simulated so far.
    #[must_use]
    pub const fn time(&self) -> f64 {
        self.t
    }

    /// The topology.
    #[must_use]
    pub const fn topology(&self) -> &Topology {
        &self.topology
    }

    fn fault_on(&self, svc: usize, t: f64) -> impl Iterator<Item = &FaultKind> {
        let name = &self.topology.services[svc].name;
        self.faults.iter().filter(move |f| f.active(t) && &f.target == name).map(|f| &f.kind)
    }

    fn cpu_factor(&self, svc: usize, t: f64) -> f64 {
        self.fault_on(svc, t).map(|k| if let FaultKind::Cpu { factor } = k { *factor } else { 1.0 }).fold(1.0, f64::max)
    }

    fn is_down(&self, svc: usize, t: f64) -> bool {
        t < self.state[svc].down_until || self.fault_on(svc, t).any(|k| matches!(k, FaultKind::Crash))
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Advances the simulation by `dt` seconds and returns the telemetry produced.
    #[allow(clippy::cast_precision_loss)]
    pub fn step(&mut self, dt: f64) -> Batch {
        let mut batch = Batch::default();
        let (t0, t1) = (self.t, self.t + dt);
        let phase = std::f64::consts::TAU * t0 / self.workload.period_s.max(1.0);
        let rate = (self.workload.rate * (1.0 + self.workload.modulation * phase.sin())).max(0.0);
        let arrivals = self.rng.poisson(rate * dt);
        let mut starts: Vec<f64> = (0..arrivals).map(|_| self.rng.uniform(t0, t1)).collect();
        starts.sort_by(f64::total_cmp);
        for start in starts {
            let mut pick = self.rng.f64() * self.total_weight;
            let mut entry = &self.topology.entries[0];
            for e in &self.topology.entries {
                if pick < e.weight {
                    entry = e;
                    break;
                }
                pick -= e.weight;
            }
            let (svc, op) = (entry.service, entry.op);
            let trace = (u128::from(self.rng.next_u64()) << 64) | u128::from(self.rng.next_u64()) | 1;
            self.execute(svc, op, start, trace, 0, 0, &mut batch);
        }
        self.advance_state(t1, dt, &mut batch);
        self.t = t1;
        batch
    }

    /// Updates load estimates, memory, restarts; emits metrics when due.
    #[allow(clippy::cast_precision_loss)]
    fn advance_state(&mut self, t: f64, dt: f64, batch: &mut Batch) {
        let alpha = 1.0 - (-dt / SMOOTHING_S).exp();
        for i in 0..self.state.len() {
            let capacity = self.topology.services[i].capacity;
            let received = self.state[i].tick_work / dt;
            let st = &mut self.state[i];
            st.work_rate += alpha * (received - st.work_rate);
            st.backlog = (st.backlog + (received - capacity) * dt).max(0.0);
            st.tick_work = 0.0;
        }
        for i in 0..self.state.len() {
            let leak: f64 = self
                .fault_on(i, t)
                .map(|k| if let FaultKind::MemoryLeak { mb_per_s } = k { *mb_per_s } else { 0.0 })
                .sum();
            let base = self.topology.services[i].memory_mb;
            let st = &mut self.state[i];
            if t >= st.down_until {
                st.memory_mb += leak * dt;
                // Leak-free services drift back to their baseline.
                if leak == 0.0 {
                    st.memory_mb += (base - st.memory_mb) * (1.0 - (-dt / 60.0).exp());
                }
                if st.memory_mb > base * MEMORY_LIMIT_FACTOR {
                    // OOM kill: the process restarts with an empty heap.
                    st.down_until = t + RESTART_S;
                    st.memory_mb = base;
                    batch.logs.push(SimLog {
                        service: i,
                        t,
                        error: true,
                        body: "java.lang.OutOfMemoryError: Java heap space".into(),
                    });
                }
            }
            st.memory_mb = st.memory_mb.max(base * 0.5);
        }
        if t >= self.next_metrics {
            self.next_metrics = t + self.workload.metric_interval_s;
            for i in 0..self.state.len() {
                if self.is_down(i, t) {
                    continue; // a dead process exports nothing
                }
                let capacity = self.topology.services[i].capacity;
                let cpu_extra = if self.cpu_factor(i, t) > 1.0 { 0.35 } else { 0.0 };
                let util = (self.state[i].work_rate / capacity).min(1.0);
                let cpu = (util * 0.8 + 0.03 + cpu_extra + self.rng.normal(0.0, 0.01)).clamp(0.0, 1.0);
                let mem = self.state[i].memory_mb * (1.0 + self.rng.normal(0.0, 0.004));
                batch.metrics.push(SimMetric { service: i, name: "container.cpu.utilization", t, value: cpu });
                batch.metrics.push(SimMetric {
                    service: i,
                    name: "container.memory.usage",
                    t,
                    value: mem * 1_048_576.0,
                });
            }
        }
    }

    /// Executes one call to `svc/op` arriving at `start`; returns `(end, error)`.
    #[allow(clippy::too_many_arguments, clippy::cast_precision_loss)]
    fn execute(
        &mut self,
        svc: usize,
        op: usize,
        start: f64,
        trace: u128,
        parent: u64,
        depth: usize,
        batch: &mut Batch,
    ) -> (f64, bool) {
        if depth > 16 {
            return (start, true);
        }
        let service = &self.topology.services[svc];
        let capacity = service.capacity;
        let operation = service.ops[op].clone();
        let cpu = self.cpu_factor(svc, start);
        let work = self.rng.lognormal_median_p99(operation.work_ms * MS, operation.tail) * cpu;
        self.state[svc].tick_work += work;
        let mean_work = operation.work_ms * MS * cpu;
        let st = &self.state[svc];
        let rho = st.work_rate / capacity;
        let wait = queue_wait(rho, capacity, mean_work) + st.backlog / capacity;
        let span_id = self.id();
        self.state[svc].requests += 1;

        let mut t = start + wait + work * 0.4;
        let mut failed_child = None;
        let mut group_start = t;
        let mut group_end = t;
        for call in &operation.calls {
            if !self.rng.chance(call.probability) {
                continue;
            }
            let call_start = if call.parallel { group_start } else { group_end };
            if !call.parallel {
                group_start = group_end;
            }
            let (end, error, cause) =
                self.call(svc, call.service, call.op, call.timeout_ms * MS, call_start, trace, span_id, depth, batch);
            group_end = group_end.max(end);
            if error && failed_child.is_none() {
                failed_child = Some(cause);
            }
            t = group_end;
        }
        t = t.max(group_end) + work * 0.6;

        let own_error_rate = operation.error_rate
            + self
                .fault_on(svc, start)
                .map(|k| if let FaultKind::Errors { rate } = k { *rate } else { 0.0 })
                .sum::<f64>();
        let own_error = self.rng.chance(own_error_rate);
        let propagated = failed_child.is_some() && self.rng.chance(0.9);
        let error = own_error || propagated;
        if error {
            self.state[svc].errors += 1;
            let body = if own_error {
                format!(
                    "{} failed: java.lang.IllegalStateException: invariant violated for request {}",
                    operation.name,
                    self.rng.below(1_000_000)
                )
            } else {
                format!("{} failed: {}", operation.name, failed_child.unwrap_or_default())
            };
            batch.logs.push(SimLog { service: svc, t, error: true, body });
        } else if self.rng.chance(self.workload.log_sampling) {
            batch.logs.push(SimLog {
                service: svc,
                t,
                error: false,
                body: format!("{} completed in {:.1} ms", operation.name, (t - start) * 1e3),
            });
        }
        batch.spans.push(SimSpan {
            trace_id: trace,
            span_id,
            parent_id: parent,
            service: svc,
            name: operation.name,
            server: true,
            start,
            end: t,
            error,
            db: None,
            peer: None,
        });
        (t, error)
    }

    /// A client call from `caller` to `target`; returns `(end, error, cause)`.
    #[allow(clippy::too_many_arguments)]
    fn call(
        &mut self,
        caller: usize,
        target: usize,
        op: usize,
        timeout: f64,
        start: f64,
        trace: u128,
        parent: u64,
        depth: usize,
        batch: &mut Batch,
    ) -> (f64, bool, String) {
        let client_id = self.id();
        let target_name = self.topology.services[target].name.clone();
        let delay: f64 =
            self.fault_on(target, start).map(|k| if let FaultKind::Delay { ms } = k { *ms * MS } else { 0.0 }).sum();
        let loss: f64 =
            self.fault_on(target, start).map(|k| if let FaultKind::Loss { rate } = k { *rate } else { 0.0 }).sum();
        let mut net = 0.3 * MS + self.rng.exponential(1.0 / (0.2 * MS)) + delay;
        if self.rng.chance(loss) {
            // A lost packet costs a retransmission timeout (200 ms, doubling).
            let mut rto = 0.2;
            while self.rng.chance(loss) && rto < 3.2 {
                net += rto;
                rto *= 2.0;
            }
            net += rto;
        }

        let (callee_end, callee_error, cause) = if self.is_down(target, start) {
            (start + 2.0 * MS, true, format!("connection refused by {target_name}"))
        } else if self.topology.services[target].kind == Kind::Datastore {
            let op_work = self.topology.services[target].ops[op].work_ms * MS * self.cpu_factor(target, start);
            let work = self.rng.lognormal_median_p99(op_work, 4.0);
            self.state[target].tick_work += work;
            let st = &self.state[target];
            let cap = self.topology.services[target].capacity;
            let wait = queue_wait(st.work_rate / cap, cap, op_work) + st.backlog / cap;
            let failed = self.rng.chance(
                self.fault_on(target, start)
                    .map(|k| if let FaultKind::Errors { rate } = k { *rate } else { 0.0 })
                    .sum::<f64>(),
            );
            (start + net + wait + work, failed, format!("{target_name} query failed"))
        } else {
            let (end, error) = self.execute(target, op, start + net / 2.0, trace, client_id, depth + 1, batch);
            (end + net / 2.0, error, format!("upstream {target_name} returned an error"))
        };

        let (end, error, cause) = if callee_end - start > timeout {
            (start + timeout, true, format!("timeout calling {target_name} after {:.0} ms", timeout * 1e3))
        } else {
            (callee_end, callee_error, cause)
        };
        let db = (self.topology.services[target].kind == Kind::Datastore).then(|| target_name.clone());
        batch.spans.push(SimSpan {
            trace_id: trace,
            span_id: client_id,
            parent_id: parent,
            service: caller,
            name: format!("call {target_name}"),
            server: false,
            start,
            end,
            error,
            db,
            peer: Some(target_name),
        });
        (end, error, cause)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p95(xs: &mut [f64]) -> f64 {
        xs.sort_by(f64::total_cmp);
        xs[xs.len() * 95 / 100]
    }

    fn latencies(batch: &Batch, svc: usize) -> Vec<f64> {
        batch
            .spans
            .iter()
            .filter(|s| s.server && s.service == svc && s.parent_id == 0)
            .map(|s| s.end - s.start)
            .collect()
    }

    fn run(faults: Vec<Fault>, seconds: usize) -> (Simulation, Vec<Batch>) {
        let mut sim = Simulation::new(Topology::shop(), Workload::default(), faults, 1).unwrap();
        let batches = (0..seconds).map(|_| sim.step(1.0)).collect();
        (sim, batches)
    }

    #[test]
    fn traffic_and_spans_are_consistent() {
        let (_, batches) = run(vec![], 120);
        let roots: usize = batches.iter().map(|b| b.spans.iter().filter(|s| s.parent_id == 0).count()).sum();
        // ~40 req/s for two minutes.
        assert!((4_000..5_800).contains(&roots), "{roots}");
        for b in &batches {
            for s in &b.spans {
                assert!(s.end >= s.start, "{s:?}");
            }
        }
        let metrics: usize = batches.iter().map(|b| b.metrics.len()).sum();
        assert!(metrics > 0);
    }

    #[test]
    fn cpu_fault_raises_latency_non_linearly_and_propagates() {
        let fault =
            Fault { target: "cart".into(), kind: FaultKind::Cpu { factor: 6.0 }, start_s: 120.0, duration_s: 120.0 };
        let (sim, batches) = run(vec![fault], 240);
        let cart = sim.topology().service_index("cart").unwrap();
        let before: Vec<f64> = batches[60..120]
            .iter()
            .flat_map(|b| b.spans.iter().filter(|s| s.server && s.service == cart).map(|s| s.end - s.start))
            .collect();
        let during: Vec<f64> = batches[150..240]
            .iter()
            .flat_map(|b| b.spans.iter().filter(|s| s.server && s.service == cart).map(|s| s.end - s.start))
            .collect();
        let (mut b, mut d) = (before, during);
        assert!(p95(&mut d) > 3.0 * p95(&mut b), "before {} during {}", p95(&mut b), p95(&mut d));
        // The entry point is slower too.
        let mut fe_before: Vec<f64> = batches[60..120].iter().flat_map(|x| latencies(x, 0)).collect();
        let mut fe_during: Vec<f64> = batches[150..240].iter().flat_map(|x| latencies(x, 0)).collect();
        assert!(p95(&mut fe_during) > p95(&mut fe_before));
    }

    #[test]
    fn crash_causes_connection_errors_upstream_and_silence() {
        let fault = Fault { target: "payment".into(), kind: FaultKind::Crash, start_s: 30.0, duration_s: 60.0 };
        let (sim, batches) = run(vec![fault], 90);
        let payment = sim.topology().service_index("payment").unwrap();
        let server_spans_during: usize =
            batches[40..90].iter().map(|b| b.spans.iter().filter(|s| s.server && s.service == payment).count()).sum();
        assert_eq!(server_spans_during, 0);
        let refused = batches[40..90]
            .iter()
            .flat_map(|b| &b.logs)
            .filter(|l| l.body.contains("connection refused by payment"))
            .count();
        assert!(refused > 0);
    }

    #[test]
    fn memory_leak_ends_in_restarts() {
        let fault = Fault {
            target: "cart".into(),
            kind: FaultKind::MemoryLeak { mb_per_s: 5.0 },
            start_s: 10.0,
            duration_s: 300.0,
        };
        let (_, batches) = run(vec![fault], 200);
        let ooms = batches.iter().flat_map(|b| &b.logs).filter(|l| l.body.contains("OutOfMemoryError")).count();
        assert!(ooms >= 1);
    }

    #[test]
    fn simulation_is_deterministic() {
        let (_, a) = run(vec![], 30);
        let (_, b) = run(vec![], 30);
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_unknown_fault_targets() {
        let f = Fault { target: "nope".into(), kind: FaultKind::Crash, start_s: 0.0, duration_s: 1.0 };
        assert!(Simulation::new(Topology::shop(), Workload::default(), vec![f], 1).is_err());
    }
}
