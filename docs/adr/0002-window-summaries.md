# 0002. Mergeable window summaries as the unit of state

**Status:** accepted

## Context

Raw telemetry is far too large to keep: a medium system produces millions of
spans per minute. The analysis needs, per service and time window, request
counts, latency quantiles, error counts, call edges, log counts and metric
values. It also needs to run on several machines when one is not enough, to
survive restarts, and to be replayable for tests.

## Decision

Every closed window is reduced to a `WindowSummary` that is a **commutative
monoid**: `merge` is associative and commutative, and the empty summary is
its identity. Latency distributions are DDSketches (mergeable, 1 % relative
error); counts add; metric aggregates carry sums and counts. Everything
downstream of the aggregator (series, detectors, incidents, analysis)
consumes summaries only.

## Consequences

* **Distribution is simple**: edges compute summaries of their share of the
  traffic and cores merge them; the order of arrival does not matter
  ([0005](0005-edge-core-without-consensus.md)).
* **Memory is bounded** by the number of services and series, not by the
  traffic.
* **Tests are strong**: the monoid laws are property-tested, and the
  distributed simulation checks that cores compute exactly the single-node
  summaries.
* Anything not representable in a mergeable form is unavailable to the
  analysis. Exact quantiles are the main loss (1 % relative error instead);
  per-trace detail is kept only as aggregates (local time, error origins,
  edges).

## Alternatives

* **Storing raw spans** (like tracing backends do) and querying them at
  analysis time: orders of magnitude more storage, and analyses that get
  slower as traffic grows.
* **Non-mergeable summaries** (exact quantiles, t-digest merged
  approximately): simpler locally, but they rule out a clean distributed
  mode.
