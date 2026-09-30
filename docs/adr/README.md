# Architecture decision records

Each record states a decision, the context that forced it, the alternatives
that were rejected and what the decision costs. They are short on purpose.

| # | decision |
|---|---|
| [0001](0001-rust-core-python-harness.md) | Rust for the engine, Python only for evaluation, one implementation shared by both |
| [0002](0002-window-summaries.md) | Mergeable window summaries as the unit of state |
| [0003](0003-single-threaded-deterministic-engine.md) | A single-threaded, deterministic engine behind an actor |
| [0004](0004-linear-listwise-ranker.md) | A linear listwise ranker with an expert prior, not a deep model |
| [0005](0005-edge-core-without-consensus.md) | Edge/core distribution without consensus |
| [0006](0006-selective-otlp-decoding.md) | Selective OTLP decoding, verified against the reference decoder |
| [0007](0007-no-llm-in-the-decision-path.md) | No language model in the decision path |
