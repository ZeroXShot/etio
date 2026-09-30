# 0003. A single-threaded, deterministic engine behind an actor

**Status:** accepted

## Context

The engine updates thousands of series, detectors and incidents per window.
Making this state concurrently mutable (locks per series, concurrent maps)
would make the code harder to reason about, the results dependent on thread
scheduling, and bugs hard to reproduce. At the same time, ingestion must use
several cores, and the API must stay responsive while ingestion is saturated.

## Decision

* The engine is a plain single-threaded state machine. It never reads a
  clock: time is an input (`advance(now)`), so the same inputs always produce
  the same outputs, including incident IDs.
* In the server it runs on a dedicated OS thread that owns it (an actor).
  Decoding, the expensive part of ingestion, happens in parallel on the async
  runtime and hands batches to the engine over a **bounded** queue.
* Queries and commands use a separate control channel that the actor always
  serves first.
* A full queue is reported to clients with the OTLP backpressure signals
  (gRPC `UNAVAILABLE`, HTTP `429`/`503` with `Retry-After`), so exporters
  back off instead of the server running out of memory.

## Consequences

* Deterministic tests: the simulator, the integration tests and the
  distributed-mode simulation replay hours of traffic in seconds and assert
  exact outcomes.
* One engine thread bounds throughput per node (more than half a million
  spans per second, measured with `etio sim bench`); beyond that, the
  edge/core mode scales out ([0005](0005-edge-core-without-consensus.md)).
* A long analysis delays window processing on that node; analyses are
  therefore bounded (candidate and signal caps) and measured
  (`etio_tick_seconds`).

## Alternatives

* **Sharded engine threads by service**: breaks cross-service analysis
  (incidents and RCA need all services) and adds coordination.
* **Shared state with fine-grained locks**: nondeterministic, and contention
  on popular series.
