# 0006. Selective OTLP decoding, verified against the reference decoder

**Status:** accepted

## Context

Protobuf decoding dominates ingestion cost. Generated decoders (`prost`)
materialise every field of every span (attributes, events, links) as owned
strings and vectors, although Etio needs a small subset: identifiers, times,
kind, status, service name and a handful of attributes.

## Decision

The receivers decode OTLP directly from the wire format, reading only the
needed fields and borrowing strings from the request buffer. The generated
`prost` types are still compiled from the vendored OTLP protos and are used
as the **reference**: differential property tests encode random requests with
`prost` (including reordered fields, unknown fields and empty messages) and
check that the selective decoder extracts exactly what the reference decoding
would.

## Consequences

* About 4× faster decoding (1.41 M spans/s per core against 0.34 M with
  `prost`, `cargo bench -p etio-otlp`).
* More code to maintain than a generated decoder; the differential tests and
  the vendored, pinned protos (OTLP v1.11.1) contain that risk.
* OTLP/JSON is converted to protobuf first (via `prost` and serde), since it
  is rare and not performance-critical.

## Alternatives

* **`prost` everywhere**: simplest; 4× the CPU per span.
* **A zero-copy protobuf library**: none covered OTLP's shapes without
  generated code of similar cost, and a hand decoder of the needed subset is
  small.
