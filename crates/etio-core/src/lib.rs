//! Core data model and streaming statistics for Etio.
//!
//! This crate has no I/O and no async code. Everything in it is deterministic
//! given its inputs, which is what lets the engine be replayed bit-for-bit in
//! tests, in simulation and in the evaluation harness.
//!
//! * [`time`]: event-time primitives (timestamps, resolutions, window indices).
//! * [`symbol`]: a bounded, thread-safe string interner.
//! * [`signal`]: the vocabulary used to describe telemetry series.
//! * [`sketch`]: mergeable quantile sketches.
//! * [`stats`]: robust statistics, extreme-value theory and change detection.
//! * [`rng`]: a stable, versioned pseudo-random generator and distributions.

pub mod rng;
pub mod signal;
pub mod sketch;
pub mod stats;
pub mod symbol;
pub mod time;

pub use signal::{Direction, SignalCategory};
pub use symbol::{Interner, Sym};
pub use time::{Resolution, Timestamp};
