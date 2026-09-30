# Architecture

Etio is one binary (`etio`) with four roles: a standalone server, an edge or
a core of a distributed deployment, and a set of offline tools (simulation,
offline analysis, health probe). The analysis code is also exposed to
Python (`etio._native`) so the evaluation harness runs exactly the code the
server runs.

```
                 OTLP gRPC / HTTP (protobuf, JSON)
                              │
             ┌────────────────▼─────────────────┐
             │ receivers (tonic / axum)          │  auth, size limits, gzip/zstd,
             │  selective zero-copy decoding     │  backpressure (UNAVAILABLE / 429)
             └────────────────┬─────────────────┘
                              │ bounded queue (batches)
             ┌────────────────▼─────────────────┐
             │ engine actor (one OS thread)      │◄── control channel (queries,
             │                                   │    snapshots), served first
             │  trace assembler ─► trace analysis│
             │  (skew fix, self time, error      │
             │   origins, call edges)            │
             │  log templates (Drain)            │
             │  metric deltas (cumulative→rate)  │
             │            │                      │
             │            ▼                      │
             │  window aggregator ──► WindowSummary (mergeable)
             │            │                      │
             │            ▼                      │
             │  series store + detectors         │  robust z, SPOT/EVT, CUSUM,
             │            │                      │  conformal p-values
             │            ▼                      │
             │  incident policy ─► RCA ranker    │  features, linear ranker,
             └────────────┬──────────────────────┘  random walk, explanations
                          │ events
      ┌───────────────────┼────────────────────┐
      ▼                   ▼                    ▼
  REST API + SSE      webhooks (HMAC)     SQLite (incidents, feedback)
  web UI, /metrics                        snapshots (atomic, CRC, lz4)
```

## Crates

| crate | responsibility | depends on |
|---|---|---|
| `etio-core` | time, interning, signal taxonomy, statistics (robust estimators, sliding order statistics, EVT, CUSUM, DDSketch), PRNG | – |
| `etio-analysis` | anomaly detectors, service graph, RCA (scoring, features, ranker, explanations) | core |
| `etio-pipeline` | span model, trace analysis, log templates, offline batch conversion | core |
| `etio-engine` | streaming engine: assembly, aggregation, store, incidents, snapshots, cluster protocol | core, analysis, pipeline |
| `etio-otlp` | vendored OTLP protos, selective decoders, OTLP/JSON | core, engine |
| `etio-server` | the binary: configuration, receivers, actor, API, persistence, auth, TLS, cluster transport | all |
| `etio-sim` | request-level simulator of microservice systems with fault injection | core, pipeline, engine |
| `etio-py` | Python bindings (PyO3), excluded from default builds | core, analysis, pipeline |

The dependency graph is acyclic and the analysis crates have no I/O, so they
are testable in isolation and usable as libraries.

## The engine

The engine is a deterministic state machine driven by two kinds of input:
telemetry and time (`advance(now)`). It never reads a clock itself, which is
what makes it testable: the same inputs always produce the same incidents,
and the simulator and the integration tests replay hours of traffic in
seconds.

**Event time and windows.** Observations are bucketed by their own
timestamps into fixed windows (10 s by default). A window closes once the
clock has passed its end plus `lateness`; data arriving later is counted and
dropped. Traces are assembled first (a trace is complete when it has been
idle for `trace_timeout`), so window statistics include the service-local
time and error origins that only a whole trace reveals.

**Window summaries are a commutative monoid.** A closed window becomes a
`WindowSummary`: per-service request counts and latency sketches (DDSketch,
mergeable with bounded relative error), call edges, error origins, log
counts and metric aggregates. `merge` is associative and commutative with
the empty summary as identity (property-tested). Everything downstream
(series, detectors, incidents, analysis) consumes only summaries. This single
design decision is what makes the distributed mode simple: edges produce
summaries, cores merge them.

**Series and detectors.** Each summary becomes one value per series
(`service × signal`), written to a ring buffer holding `retention` of
history. Each series has its own detector (see
[algorithms](algorithms.md)); a service is anomalous in a window when one of
its series is confirmed anomalous for `confirm_windows` windows with enough
surprise.

**Incidents.** The incident policy opens an incident when at least
`min_services` services are anomalous together, when one service is
severely so, or when an entry point's *user-facing* signals (latency,
errors, traffic: what users experience) are anomalous. It records which
signal of each service triggered it, schedules root-cause analyses (`rca_delay` after opening, then every
`rca_interval`), and resolves it after `resolve_after` of quiet. Incident IDs
derive from the data (start time and hash of the trigger), so a replay
produces the same IDs.

**Memory is bounded everywhere**: interned strings (sharded, capped), series
count, open windows, buffered spans per trace and in total, log templates per
service (LRU), counter streams. Every bound has a counter exported as a
metric, so saturation is visible rather than silent.

## The server

**One engine thread, two channels.** The engine is single-threaded by design
(no locks on the hot path, deterministic). Receivers decode on the async
runtime's threads, which parallelises the expensive part, and hand batches to
the engine over a **bounded** queue. When the queue is full, OTLP clients get
the protocol's own backpressure signals (`UNAVAILABLE` over gRPC, `429`/`503`
with `Retry-After` over HTTP), which the OpenTelemetry SDKs and Collector
honour by retrying with backoff. Queries use a separate control channel that
is always served first, so the API stays responsive under saturation.

**Decoding.** The OTLP decoders read only the fields Etio uses, directly from
the protobuf wire format, without materialising the generated message types:
about 4× faster than decoding with `prost` (see `cargo bench -p etio-otlp`).
They are verified against `prost` by differential property tests, including
field reordering and unknown fields.

**Persistence.** Snapshots of the engine state (postcard, lz4, CRC32,
written to a temporary file and renamed) are taken periodically and at
shutdown. A restored engine marks the windows whose data was in flight at
snapshot time as gaps, so it does not raise alarms on incomplete data.
Incidents and operator feedback also go to SQLite for history beyond the
in-memory retention.

**Security.** See [security](security.md): bearer tokens compared in constant
time, TLS and mutual TLS with rustls, secrets read from files, request and
decompression limits, HMAC-signed webhooks.

## Distributed mode

A single engine thread handles more than half a million spans per second
(the scale benchmark, `etio sim bench`, sustains 1.7 M spans/s on three
cores, each running a simulator and an engine). Beyond that, or to aggregate
telemetry close to where it is produced, Etio runs as **edges** and
**cores**:

```
 apps ─► OTel Collector ──(trace-ID load balancing)──► edge-0 ─┐  summaries (gRPC stream,
                                                  └──► edge-1 ─┼─► core-0   at-least-once,
                                                               └─► core-1   deduplicated)
```

* An **edge** runs the receivers, the trace assembler and the aggregator,
  and ships each closed `WindowSummary` to every core. It runs no detection.
  All spans of a trace must reach the same edge, which the Collector's
  `loadbalancing` exporter does by trace ID.
* A **core** merges the summaries of all edges window by window and runs
  detection, incidents, analysis and the API. Cores are independent
  replicas: each receives everything, so any core can serve the API and
  losing one loses nothing.

There is **no consensus protocol**, because nothing needs agreement: merging
is commutative, so cores that receive the same summaries compute the same
state regardless of arrival order.

**Protocol** (`proto/etio/cluster/v1/cluster.proto`). Each edge keeps one
bidirectional gRPC stream per core. Summaries carry a sequence number within
the edge's *epoch* (its start time, so a restarted edge is a new
incarnation). The edge retains summaries in a bounded outbox until every core
has acknowledged them. The core acknowledges cumulatively and applies each
`(edge, epoch, sequence)` exactly once. The protocol logic lives in sans-I/O
state machines (`etio_engine::cluster::{Outbox, Collector}`); the gRPC
transport around them is thin.

**When is a window complete?** A core releases window *w* once every live
edge has reported *w* or a later window (watermarks). An edge that stays
silent does not block the others forever: *w* is released anyway when some
edge has reported a window `deadline` later **in event time**, and an edge
silent for longer than `liveness` (wall time) stops being waited for.
Measuring the deadline in event time makes release decisions independent of
processing speed, so replays behave like live traffic. Summaries that arrive
after their window was released are counted as `late`.

**Deterministic simulation testing.** The protocol was developed against a
simulation that runs 1–4 edges and 2 cores over a network that drops,
duplicates and delays messages and resets connections, across 200 seeds per
test run. The invariant: every core releases every window exactly once, in
order, equal to what a single node would have computed. The simulation found
three requirements that the first design missed:

1. a core must wait for a short start-up grace period before releasing
   windows, or an edge that connects a moment later loses its first windows;
2. an edge must rewind to the acknowledged sequence when the core reports a
   gap, instead of continuing from its send cursor;
3. an edge needs a retransmission timer: when a message and its
   retransmission trigger are both lost, nothing else would resend it.

Two more came from the integration tests with real streams:

4. a stopping core must close its open streams, otherwise a lingering
   connection keeps acknowledging summaries that the next incarnation will
   never see;
5. a restarted core knows nothing about the edges. The edge's hello therefore
   carries the first sequence it can still send, which the new core adopts
   as its starting point (and which, from a known edge, declares summaries
   the outbox had to drop, counted as `lost` instead of blocking the stream).

**Failure semantics.**

| failure | effect |
|---|---|
| core unreachable | edges retain summaries (`outbox_capacity` windows, 12 h at 10 s by default) and deliver them on reconnection |
| core restart | state restored from its snapshot; windows applied after the last snapshot are lost as in standalone mode, and marked as gaps |
| edge restart | its buffered traces and open windows are lost (at most `lateness + trace_timeout` of its share of the data); the new epoch starts cleanly |
| edge dead | its windows are released without it after `deadline` of event time, then it is no longer waited for |
| outbox overflow | oldest summaries dropped, declared to the cores, counted (`etio_cluster_summaries{event="lost"}`) |

## Why these choices

The main decisions are recorded as ADRs in [`docs/adr`](adr/).
