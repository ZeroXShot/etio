# 0005. Edge/core distribution without consensus

**Status:** accepted

## Context

One engine thread handles a large system but not an arbitrarily large one,
and some deployments want aggregation close to where telemetry is produced.
Distributed designs usually bring a consensus protocol (Raft) or a message
broker (Kafka) along, with their operational cost.

## Decision

* **Edges** assemble traces and aggregate windows; **cores** merge window
  summaries and run detection and analysis.
* Every edge sends every summary to every core. Cores are independent
  replicas: because merging is commutative
  ([0002](0002-window-summaries.md)), cores that receive the same summaries
  compute the same state, whatever the order. Nothing needs agreement, so
  there is no consensus protocol.
* Delivery is at-least-once over one gRPC stream per edge and core, with
  per-edge sequence numbers, cumulative acknowledgements and deduplication by
  `(edge, epoch, sequence)`.
* A window is released when all live edges have reported it, or when another
  edge is `deadline` ahead in **event time**.
* The protocol logic is sans-I/O and verified by deterministic simulation
  (drops, duplicates, delays, resets) before any network code was written.

## Consequences

* No broker, no quorum, no leader election; a core can be added or removed
  without coordination (a new core only needs the edges' configuration).
* Traffic to cores grows with edges × cores, but summaries are small (one per
  window per edge).
* Traces must be routed to edges by trace ID (the OpenTelemetry Collector's
  `loadbalancing` exporter does this).
* A core crash loses the windows since its last snapshot, as in standalone
  mode.

## Alternatives

* **Kafka between edges and cores**: durable and replayable, but a heavy
  dependency for a stream of small summaries that the edges can buffer
  themselves.
* **Raft-replicated core state**: solves a problem Etio does not have
  (agreement on an order of updates that commute anyway).
* **Processing-time deadlines**: simpler, but a replay or a catching-up edge
  would have its windows released early and counted late.
