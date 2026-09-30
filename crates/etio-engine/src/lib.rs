//! The deterministic streaming engine of Etio.
//!
//! * [`engine`]: the engine itself: ingestion, event-time windows,
//!   detection, incidents and root-cause analysis.
//! * [`window`]: mergeable window summaries and the window aggregator.
//! * [`assembler`]: streaming trace assembly with bounded memory.
//! * [`cluster`]: the edge/core protocol as pure state machines.
//! * [`store`]: the hot series store.
//! * [`incident`]: the incident lifecycle.
//! * [`config`]: validated configuration.
//! * [`records`]: metric and log record types.

pub mod assembler;
pub mod cluster;
pub mod config;
pub mod engine;
pub mod incident;
pub mod records;
pub mod store;
pub mod window;

pub use config::{EngineConfig, IncidentConfig, Limits};
pub use engine::{Engine, EngineSnapshot, EngineStats, Event, RestoreError, SNAPSHOT_FORMAT};
pub use incident::{Incident, IncidentStatus};
pub use records::{LogEntry, MetricKind, MetricPoint};
pub use window::WindowSummary;
