//! A deterministic simulator of microservice systems with fault injection.
//!
//! It produces the telemetry an OpenTelemetry-instrumented system would
//! emit (traces, metrics, logs) together with the ground truth of injected
//! faults, and serves three purposes:
//!
//! * **testing**: end-to-end tests of the engine and the server without
//!   containers;
//! * **load generation**: sending OTLP at a controlled rate to a server;
//! * **evaluation at scale**: systems of hundreds of services and fault
//!   types that public benchmarks do not cover (crashes, error bursts,
//!   memory leaks, packet loss).
//!
//! * [`topology`]: services, operations, call graphs, presets and generators.
//! * [`sim`]: the simulation model and fault kinds.
//! * [`telemetry`]: conversion to engine records and to OTLP requests.
//! * [`scenario`]: reproducible scenarios and their evaluation.
//! * [`bench`]: the scale benchmark over random systems.

pub mod bench;
pub mod scenario;
pub mod sim;
pub mod telemetry;
pub mod topology;

pub use scenario::{Outcome, Scenario};
pub use sim::{Batch, Fault, FaultKind, Simulation, Workload};
pub use topology::Topology;
