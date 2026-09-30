//! System topologies: services, their operations and who calls whom.

use etio_core::rng::Rng;
use serde::{Deserialize, Serialize};

/// A downstream call made by an operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Call {
    /// Index of the called service.
    pub service: usize,
    /// Index of the called operation in that service.
    pub op: usize,
    /// Probability that a request makes this call.
    pub probability: f64,
    /// Whether the call runs concurrently with the previous parallel calls.
    pub parallel: bool,
    /// Client-side timeout, milliseconds.
    pub timeout_ms: f64,
}

/// An operation (endpoint) of a service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    /// Span name.
    pub name: String,
    /// Median processing time inside the service, milliseconds.
    pub work_ms: f64,
    /// Ratio p99/median of the processing time (tail heaviness).
    pub tail: f64,
    /// Baseline probability of failing on its own.
    pub error_rate: f64,
    /// Downstream calls, in order.
    pub calls: Vec<Call>,
}

/// What kind of component a service is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// An instrumented application service.
    Service,
    /// A datastore without tracing of its own; it appears in client spans
    /// (`db.system`) and emits only infrastructure metrics.
    Datastore,
}

/// A service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Service {
    /// Service name (`service.name`).
    pub name: String,
    /// Component kind.
    pub kind: Kind,
    /// Concurrent requests the service can process (workers × cores).
    pub capacity: f64,
    /// Baseline resident memory, MiB.
    pub memory_mb: f64,
    /// Operations.
    pub ops: Vec<Operation>,
}

/// A user-facing entry operation and its share of the traffic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Service index.
    pub service: usize,
    /// Operation index.
    pub op: usize,
    /// Relative share of requests.
    pub weight: f64,
}

/// A complete system.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Topology {
    /// Services.
    pub services: Vec<Service>,
    /// Entry operations.
    pub entries: Vec<Entry>,
}

/// A topology that cannot be simulated.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TopologyError {
    /// A call or entry refers to a service or operation that does not exist.
    #[error("dangling reference: {0}")]
    Dangling(String),
    /// The call graph has a cycle, which would make requests recurse forever.
    #[error("the call graph has a cycle through `{0}`")]
    Cycle(String),
    /// No entry points.
    #[error("the topology has no entry points")]
    NoEntries,
}

fn op(name: &str, work_ms: f64, calls: Vec<Call>) -> Operation {
    Operation { name: name.into(), work_ms, tail: 4.0, error_rate: 0.0005, calls }
}

fn call(service: usize, op: usize, probability: f64, parallel: bool) -> Call {
    Call { service, op, probability, parallel, timeout_ms: 1_000.0 }
}

fn service(name: &str, capacity: f64, ops: Vec<Operation>) -> Service {
    Service { name: name.into(), kind: Kind::Service, capacity, memory_mb: 180.0, ops }
}

impl Topology {
    /// Index of a service by name.
    #[must_use]
    pub fn service_index(&self, name: &str) -> Option<usize> {
        self.services.iter().position(|s| s.name == name)
    }

    /// An online shop in the spirit of the OpenTelemetry demo: eleven
    /// services, a cache without telemetry, fan-out and sequential chains.
    #[must_use]
    pub fn shop() -> Self {
        // Indices: 0 frontend, 1 cart, 2 catalog, 3 checkout, 4 payment,
        // 5 shipping, 6 currency, 7 recommendation, 8 ad, 9 email, 10 redis.
        let services = vec![
            service(
                "frontend",
                24.0,
                vec![
                    op("GET /", 2.0, vec![call(2, 0, 1.0, true), call(8, 0, 0.8, true), call(6, 0, 1.0, true)]),
                    op("GET /product", 2.0, vec![call(2, 1, 1.0, false), call(7, 0, 1.0, true), call(6, 0, 1.0, true)]),
                    op("POST /cart", 2.0, vec![call(1, 0, 1.0, false)]),
                    op("POST /checkout", 3.0, vec![call(3, 0, 1.0, false)]),
                ],
            ),
            service(
                "cart",
                8.0,
                vec![
                    op("AddItem", 3.0, vec![call(10, 0, 1.0, false)]),
                    op("GetCart", 2.0, vec![call(10, 0, 1.0, false)]),
                ],
            ),
            service("catalog", 12.0, vec![op("ListProducts", 6.0, vec![]), op("GetProduct", 3.0, vec![])]),
            service(
                "checkout",
                8.0,
                vec![op(
                    "PlaceOrder",
                    5.0,
                    vec![
                        call(1, 1, 1.0, false),
                        call(2, 1, 1.0, false),
                        call(6, 0, 1.0, false),
                        call(5, 0, 1.0, false),
                        call(4, 0, 1.0, false),
                        call(9, 0, 1.0, false),
                    ],
                )],
            ),
            service("payment", 6.0, vec![op("Charge", 12.0, vec![])]),
            service("shipping", 6.0, vec![op("GetQuote", 4.0, vec![])]),
            service("currency", 16.0, vec![op("Convert", 0.8, vec![])]),
            service("recommendation", 8.0, vec![op("ListRecommendations", 7.0, vec![call(2, 0, 1.0, false)])]),
            service("ad", 6.0, vec![op("GetAds", 3.0, vec![])]),
            service("email", 4.0, vec![op("SendConfirmation", 9.0, vec![])]),
            Service {
                name: "redis".into(),
                kind: Kind::Datastore,
                capacity: 32.0,
                memory_mb: 512.0,
                ops: vec![op("GET", 0.4, vec![])],
            },
        ];
        let entries = vec![
            Entry { service: 0, op: 0, weight: 5.0 },
            Entry { service: 0, op: 1, weight: 8.0 },
            Entry { service: 0, op: 2, weight: 2.0 },
            Entry { service: 0, op: 3, weight: 1.0 },
        ];
        Self { services, entries }
    }

    /// A random layered system with `n` services (at least 3): entry
    /// services on top, datastores at the bottom, and calls only towards
    /// lower layers, so the call graph is acyclic.
    #[must_use]
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn layered(n: usize, seed: u64) -> Self {
        let n = n.max(3);
        let mut rng = Rng::seed_from_u64(seed).fork("topology");
        let datastores = (n / 8).max(1);
        let apps = n - datastores;
        let layers = ((apps as f64).log2().ceil() as usize).clamp(2, 6);
        // Assign app services to layers (at least one per layer, first layer small).
        let mut layer_of: Vec<usize> =
            (0..apps).map(|i| if i < layers { i } else { 1 + rng.index(layers - 1) }).collect();
        layer_of.sort_unstable();
        let mut services = Vec::with_capacity(n);
        for (i, &layer) in layer_of.iter().enumerate() {
            let name = if layer == 0 { format!("gateway-{i}") } else { format!("svc-{i}") };
            services.push(Service {
                name,
                kind: Kind::Service,
                capacity: rng.uniform(4.0, 24.0).round(),
                memory_mb: rng.uniform(120.0, 600.0),
                ops: Vec::new(),
            });
        }
        for d in 0..datastores {
            let names = ["postgres", "redis", "kafka", "mongo", "elasticsearch", "memcached"];
            let name = format!("{}-{d}", names[d % names.len()]);
            services.push(Service {
                name,
                kind: Kind::Datastore,
                capacity: 48.0,
                memory_mb: 1024.0,
                ops: vec![op("query", rng.uniform(0.3, 3.0), vec![])],
            });
        }
        // Operations and calls: each app op calls 0-3 services in deeper layers or datastores.
        for i in 0..apps {
            let n_ops = 1 + rng.index(3);
            let deeper: Vec<usize> = (0..apps).filter(|&j| layer_of[j] > layer_of[i]).collect();
            let mut ops = Vec::with_capacity(n_ops);
            for k in 0..n_ops {
                let n_calls = if deeper.is_empty() { usize::from(rng.chance(0.7)) } else { rng.index(4) };
                let mut calls = Vec::new();
                for c in 0..n_calls {
                    let to_datastore = deeper.is_empty() || rng.chance(0.25);
                    let target =
                        if to_datastore { apps + rng.index(datastores) } else { deeper[rng.index(deeper.len())] };
                    calls.push(Call {
                        service: target,
                        op: 0,
                        probability: rng.uniform(0.5, 1.0),
                        parallel: c > 0 && rng.chance(0.4),
                        timeout_ms: 2_000.0,
                    });
                }
                ops.push(Operation {
                    name: format!("op-{k}"),
                    work_ms: rng.lognormal_median_p99(3.0, 4.0).min(40.0),
                    tail: rng.uniform(2.5, 6.0),
                    error_rate: 0.0005,
                    calls,
                });
            }
            services[i].ops = ops;
        }
        // Every service must be reachable from an entry point: a fault in a
        // service nobody calls has no symptom to detect. Connect unreachable
        // services from a random reachable service of a shallower layer.
        loop {
            let mut reachable = vec![false; n];
            let mut stack: Vec<usize> = (0..apps).filter(|&i| layer_of[i] == 0).collect();
            while let Some(u) = stack.pop() {
                if std::mem::replace(&mut reachable[u], true) {
                    continue;
                }
                for o in &services[u].ops {
                    stack.extend(o.calls.iter().map(|c| c.service));
                }
            }
            let Some(orphan) = (0..n).find(|&i| !reachable[i]) else { break };
            let depth = if orphan < apps { layer_of[orphan] } else { layers };
            let parents: Vec<usize> = (0..apps).filter(|&j| reachable[j] && layer_of[j] < depth).collect();
            let parent = parents[rng.index(parents.len())];
            let op = rng.index(services[parent].ops.len());
            services[parent].ops[op].calls.push(Call {
                service: orphan,
                op: 0,
                probability: 1.0,
                parallel: false,
                timeout_ms: 2_000.0,
            });
        }
        // Calls target op 0; make sure op indices are valid for app targets.
        let entries = (0..apps)
            .filter(|&i| layer_of[i] == 0)
            .flat_map(|i| (0..services[i].ops.len()).map(move |op| (i, op)))
            .map(|(service, op)| Entry { service, op, weight: 1.0 })
            .collect();
        Self { services, entries }
    }

    /// Checks references and acyclicity.
    ///
    /// # Errors
    /// Returns the first problem found.
    pub fn validate(&self) -> Result<(), TopologyError> {
        if self.entries.is_empty() {
            return Err(TopologyError::NoEntries);
        }
        let check = |s: usize, o: usize, what: &str| -> Result<(), TopologyError> {
            let svc = self.services.get(s).ok_or_else(|| TopologyError::Dangling(format!("{what}: service {s}")))?;
            svc.ops.get(o).map(|_| ()).ok_or_else(|| TopologyError::Dangling(format!("{what}: {}#{o}", svc.name)))
        };
        for e in &self.entries {
            check(e.service, e.op, "entry")?;
        }
        for s in &self.services {
            for o in &s.ops {
                for c in &o.calls {
                    check(c.service, c.op, &s.name)?;
                }
            }
        }
        // Cycle detection over services (DFS with colours).
        let n = self.services.len();
        let mut colour = vec![0u8; n];
        fn visit(t: &Topology, u: usize, colour: &mut [u8]) -> Result<(), TopologyError> {
            colour[u] = 1;
            for o in &t.services[u].ops {
                for c in &o.calls {
                    match colour[c.service] {
                        1 => return Err(TopologyError::Cycle(t.services[c.service].name.clone())),
                        0 => visit(t, c.service, colour)?,
                        _ => {}
                    }
                }
            }
            colour[u] = 2;
            Ok(())
        }
        for u in 0..n {
            if colour[u] == 0 {
                visit(self, u, &mut colour)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_valid() {
        assert_eq!(Topology::shop().validate(), Ok(()));
        for n in [3, 10, 30, 100, 300] {
            for seed in 0..5 {
                let t = Topology::layered(n, seed);
                assert_eq!(t.services.len(), n, "n={n}");
                assert_eq!(t.validate(), Ok(()), "n={n} seed={seed}");
            }
        }
    }

    #[test]
    fn every_service_is_reachable() {
        for n in [10, 30, 100] {
            let t = Topology::layered(n, 1_000_003);
            let mut seen = vec![false; n];
            let mut stack: Vec<usize> = t.entries.iter().map(|e| e.service).collect();
            while let Some(u) = stack.pop() {
                if !std::mem::replace(&mut seen[u], true) {
                    stack.extend(t.services[u].ops.iter().flat_map(|o| o.calls.iter().map(|c| c.service)));
                }
            }
            assert!(seen.iter().all(|&s| s), "n={n}");
        }
    }

    #[test]
    fn layered_is_deterministic() {
        assert_eq!(Topology::layered(40, 7), Topology::layered(40, 7));
        assert_ne!(Topology::layered(40, 7), Topology::layered(40, 8));
    }

    #[test]
    fn detects_cycles_and_dangling_references() {
        let mut t = Topology::shop();
        t.services[2].ops[0].calls.push(call(3, 0, 1.0, false)); // catalog -> checkout -> catalog
        assert!(matches!(t.validate(), Err(TopologyError::Cycle(_))));
        let mut t = Topology::shop();
        t.services[0].ops[0].calls.push(call(99, 0, 1.0, false));
        assert!(matches!(t.validate(), Err(TopologyError::Dangling(_))));
        let mut t = Topology::shop();
        t.entries.clear();
        assert_eq!(t.validate(), Err(TopologyError::NoEntries));
    }
}
