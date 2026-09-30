//! Anomaly detection and root-cause ranking.
//!
//! This crate is the analytical core of Etio. It is pure computation: no I/O,
//! no clocks, no threads. The same code runs inside the streaming engine, in
//! the offline CLI and, through the Python bindings, in the evaluation
//! harness, so the numbers reported by the benchmarks are produced by exactly
//! the code that runs in production.
//!
//! * [`detect`]: streaming per-series anomaly detection.
//! * [`graph`]: the service dependency graph and random-walk scoring.
//! * [`rca`]: root-cause ranking from a window of telemetry.

pub mod detect;
pub mod graph;
pub mod rca;

pub use detect::{DetectorConfig, SeriesDetector, SeriesState};
pub use graph::ServiceGraph;
pub use rca::{RcaConfig, RcaInput, RcaResult, SeriesInput};
