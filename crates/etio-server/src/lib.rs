//! The Etio server: OTLP ingestion, streaming root-cause analysis, an HTTP
//! API and a web UI, in one binary.
//!
//! * [`config`]: layered, validated configuration.
//! * [`actor`]: the engine thread and its bounded queues.
//! * [`otlp`]: OTLP/gRPC and OTLP/HTTP receivers.
//! * [`api`]: REST API, server-sent events, Prometheus metrics, UI.
//! * [`persist`]: snapshots and the incident store.
//! * [`notify`]: signed webhooks.
//! * [`auth`], [`tls`]: access control and transport security.
//! * [`cluster`]: edge/core transport for distributed deployments.
//! * [`serve`]: assembly and graceful shutdown.
//! * [`simulate`]: simulated OpenTelemetry traffic for demos and load tests.

pub mod actor;
pub mod api;
pub mod auth;
pub mod cluster;
pub mod config;
pub mod metrics;
pub mod notify;
pub mod otlp;
pub mod persist;
pub mod serve;
pub mod simulate;
pub mod tls;

/// Selects the process-wide TLS implementation (rustls with *ring*). Every
/// TLS user in the process (listeners, webhooks, edge streams, the health
/// probe) shares it. Idempotent.
pub fn init_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
