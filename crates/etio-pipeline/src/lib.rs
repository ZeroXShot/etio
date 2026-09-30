//! Telemetry processing: from spans and log lines to per-service signals.
//!
//! * [`span`]: the compact span record decoders produce.
//! * [`trace`]: analysis of complete traces (local time, error origins,
//!   dependency edges, clock-skew correction).
//! * [`logs`]: online log template mining and severity classification.
//! * [`batch`]: offline conversion of recorded telemetry into series, with
//!   the same algorithms the streaming engine uses.

pub mod batch;
pub mod logs;
pub mod span;
pub mod trace;

pub use span::{Span, SpanKind, SpanStatus};
