# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Streaming engine: event-time windows, trace assembly with clock-skew
  correction, service-local time, error origins, client-side call edges,
  Drain log templates, cumulative-to-delta metrics, bounded memory.
- Per-series detection: robust baselines, SPOT extreme-value thresholds with
  deferred learning, winsorised CUSUM, conformal p-values for degenerate
  baselines.
- Incident lifecycle with scheduled root-cause analyses and deterministic IDs.
- Root-cause ranking: 20 scale-free features, linear listwise ranker with a
  heuristic prior (bundled model trained on RCAEval), ensemble default,
  exact contributions and plain-language reasons; BARO, N-Sigma, max-score
  and random-walk baselines.
- Server: OTLP gRPC and HTTP (protobuf, JSON, gzip, zstd) with backpressure,
  REST API, server-sent events, Prometheus metrics, SQLite history,
  snapshots, HMAC-signed webhooks, Alertmanager intake, bearer tokens,
  TLS/mTLS, event-time clock for replays.
- Distributed mode: edges and cores exchanging mergeable window summaries
  over gRPC streams, at-least-once with deduplication, event-time deadlines,
  verified by deterministic simulation.
- Web UI: incidents, candidates with contributions and evidence charts,
  dependency graph, live updates, operator feedback.
- Simulator of microservice systems with fault injection, scenario and scale
  benchmarks.
- Python bindings and evaluation harness for RCAEval, with bootstrap
  intervals, nested cross-validation and a leakage guard.
- Container image (distroless, non-root, health check), demo and cluster
  compose examples, CI, fuzz targets.

### Fixed

- Counter rates no longer depend on how exports align with windows
  (per-stream rates over the export interval).
- Host-scoped metrics (`system.*`, `node_*`) no longer open incidents.
- An entry point opens an incident on its own only through user-facing
  signals (latency, errors, traffic), not internal ones.

### Changed

- Incidents record the signal that triggered each service
  (`trigger_signals`); snapshot format 2, edge/core summary format 2.
