# Contributing

Thank you for considering a contribution. Bug reports, benchmark results on
new systems, and documentation fixes are as welcome as code.

## Development setup

| component | needs | check with |
|---|---|---|
| Rust workspace | Rust 1.98 (`rust-toolchain.toml` selects it) | `cargo test` |
| Python harness | [uv](https://docs.astral.sh/uv/) (builds the extension with maturin) | `cd python && uv run --group dev pytest` |
| Web UI | Node 24 | `cd ui && npm ci && npm test && npm run build` |

No system libraries are needed: protobuf definitions are vendored and
compiled in pure Rust, SQLite is bundled, TLS is rustls.

```sh
make lint test          # fmt, clippy, tests
make python-test
make ui
make demo               # the instrumented demo in Docker
```

Run the server against the simulator, faster than real time:

```sh
# The event clock follows telemetry timestamps, so a simulation can run 30x
# faster than real time. A shorter warm-up makes the demo converge sooner.
ETIO__CLOCK=event ETIO__ENGINE__RESOLUTION=5s ETIO__ENGINE__INCIDENT__WARMUP=8m \
  cargo run --release -- serve &
cargo run --release -- sim run --fault cpu=6:cart@12m+5m --duration 22m --speed 30
# open http://127.0.0.1:7070 (after `make ui`, with ETIO__UI__DIR=ui/dist)
```

## Expectations for changes

* **Tests with the change.** Algorithms get property tests against a batch
  or reference implementation where one exists; the engine and the cluster
  protocol are deterministic, so their tests assert exact outcomes.
* **Evaluation for analysis changes.** Anything that changes detection or
  ranking must report its effect on the benchmark (`etio-eval run`, and
  `etio-eval train` for feature changes) with the cross-validated numbers,
  including where it got worse. Results that did not pan out are worth
  documenting too: see the negative results in
  [docs/evaluation.md](docs/evaluation.md).
* **No new unbounded state.** Every buffer, map or cache on the ingest path
  needs a bound and a counter.
* **Lints are errors.** `cargo clippy --all-targets -- -D warnings` must be
  clean; `unsafe` code is not accepted.
* **Docs follow code.** Configuration keys, metrics and API changes belong
  in `docs/` in the same change.

## Commit and review

Keep commits focused and explain *why* in the message. Pull requests run CI
on Linux and macOS (Rust, Python, UI, container, licences and advisories).

## Licence

By contributing you agree that your contributions are licensed under the
Apache License 2.0, as the rest of the project.
