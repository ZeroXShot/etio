//! Mergeable quantile sketches.
//!
//! Latency percentiles cannot be averaged: the p99 of a service is not the
//! mean of the p99s of its replicas. The engine therefore aggregates latency
//! into [`DDSketch`]es, which can be merged exactly (bucket counts add up) and
//! still answer any quantile with a bounded *relative* error. Mergeability is
//! what makes sharded ingestion and the edge/core deployment possible:
//! partial sketches built on different nodes combine into the same result as
//! a single sketch fed with all the data.

mod ddsketch;

pub use ddsketch::{DDSketch, SketchError};
