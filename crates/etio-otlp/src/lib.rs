//! OpenTelemetry protocol (OTLP) support for Etio.
//!
//! * [`decode`]: selective, zero-copy decoders for OTLP/protobuf export
//!   requests (traces, metrics, logs) into engine records.
//! * [`json`]: decoders for OTLP/JSON.
//! * [`wire`]: the protobuf wire-format reader underneath.
//! * [`grpc`]: the raw-bytes gRPC codec used by the OTLP servers.
//! * [`proto`]: generated OTLP message types, for producing OTLP.
//! * [`semconv`]: the semantic-convention keys the engine reads.

pub mod decode;
pub mod grpc;
pub mod json;
pub mod proto;
pub mod semconv;
pub mod wire;
