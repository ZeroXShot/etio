# 0001. Rust for the engine, Python for evaluation, one implementation

**Status:** accepted

## Context

Etio sits on the telemetry path: it must ingest hundreds of thousands of
spans per second per core with bounded memory and predictable latency, run
for months, and ship as a single artefact. Its algorithms, however, are
developed and judged against research benchmarks (RCAEval) whose tooling is
Python.

Reimplementing the analysis in Python for evaluation would mean evaluating
code that does not run in production: the numbers would describe a
different program.

## Decision

* The engine, the analysis and the server are Rust (no garbage collector,
  no runtime, memory safety without a performance tax, one self-contained
  binary).
* The analysis crates have no I/O and are exposed to Python through PyO3
  (`etio._native`). The evaluation harness calls *the same functions* the
  server calls.
* Python is used only for evaluation and training (data loading, metrics,
  bootstrap intervals, cross-validation), never on the serving path.

## Consequences

* Benchmark results describe the shipped code. A regression in Rust shows up
  in the evaluation, and the bundled model is trained on features computed by
  the production code path.
* Contributors need a Rust toolchain even to work on the evaluation.
* `unsafe` is forbidden in the workspace (`unsafe_code = "deny"`); the only
  unsafe code is in dependencies.

## Alternatives

* **Python end to end** (NumPy/pandas): fastest to prototype, too slow and
  memory-hungry on the ingest path, and hard to ship as a service.
* **Go**: a good fit for the server, weaker for numerical code, and a garbage
  collector on the hot path.
* **Two implementations** (Python reference, Rust production): doubles the
  work and guarantees drift.
