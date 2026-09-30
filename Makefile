# Common tasks. Everything also works with the underlying tools directly.
.PHONY: all build test lint fmt ui python-test bench image demo demo-down cluster eval clean

all: lint test

build: ui
	cargo build --release -p etio-server

test:
	cargo test --locked

lint:
	cargo fmt --all --check
	cargo clippy --all-targets --locked -- -D warnings

fmt:
	cargo fmt --all
	cd python && uv run --group dev ruff format src tests

ui:
	cd ui && npm ci && npm run build

python-test:
	cd python && uv run --group dev pytest

bench:
	cargo bench -p etio-core -p etio-pipeline -p etio-otlp
	cargo run --release -p etio-server -- sim bench

image:
	docker build -t etio:dev .

# Instrumented demo shop + OpenTelemetry Collector + Etio (UI on 127.0.0.1:7070).
demo:
	docker compose -f examples/demo/docker-compose.yml up -d --build

demo-down:
	docker compose -f examples/demo/docker-compose.yml down -v

# Two edges and a core fed by the simulator.
cluster:
	test -f examples/cluster/cluster_token || (openssl rand -hex 32 > examples/cluster/cluster_token && chmod 644 examples/cluster/cluster_token)
	docker compose -f examples/cluster/docker-compose.yml up -d --build

# Downloads RCAEval (~3 GB) and reproduces the evaluation tables.
eval:
	cd python && uv run --extra baselines etio-eval fetch --logs --traces
	cd python && uv run --extra baselines etio-eval run --sources metrics traces logs --methods etio max_score baro nsigma random_walk rcaeval_baro rcaeval_nsigma
	cd python && uv run etio-eval train --out ../eval/out/model.json

clean:
	cargo clean
	rm -rf ui/dist ui/node_modules
