# Operations

## Running

```sh
etio serve                         # defaults: OTLP on :4317/:4318, API and UI on :7070
etio serve --config etio.toml      # or ETIO_CONFIG=etio.toml
etio config default > etio.toml    # every setting, with its default
etio config check etio.toml        # validate without starting
etio health                        # readiness probe (exit status), for containers
```

Point any OpenTelemetry SDK or Collector at the OTLP endpoints (gRPC or
HTTP, protobuf or JSON, gzip or zstd). No agent and no code change are
needed beyond standard OpenTelemetry instrumentation. Etio is usually one
more exporter in an existing Collector pipeline:

```yaml
exporters:
  otlp/etio:
    endpoint: etio:4317
    tls: { insecure: true }   # or a CA, see Security
service:
  pipelines:
    traces:  { receivers: [otlp], processors: [batch], exporters: [otlp/etio, ...] }
    metrics: { receivers: [otlp], processors: [batch], exporters: [otlp/etio, ...] }
    logs:    { receivers: [otlp], processors: [batch], exporters: [otlp/etio, ...] }
```

Traces are the most valuable signal (call graph, local time, error origins,
client-side edges). Metrics add resource saturation (CPU, memory,
connections). Logs add error and novelty rates. Each is optional.

## Configuration

Configuration is layered: defaults, then the TOML file, then environment
variables, then command-line flags. Every key has an environment form:
`ETIO__` + the path in upper case with `__` between levels.

| key | default | meaning |
|---|---|---|
| `listen.otlp_grpc` / `otlp_http` / `api` | `0.0.0.0:4317` / `:4318` / `:7070` | listeners; remove one to disable it |
| `engine.resolution` | `10s` | window width; the time resolution of detection |
| `engine.lateness` | `20s` | how long a window waits for late data |
| `engine.trace_timeout` | `8s` | idle time after which a trace is complete |
| `engine.retention` | `2h` | in-memory history per series (analysis look-back) |
| `engine.incident.min_services` | `2` | services anomalous together to open an incident |
| `engine.incident.severe_surprise` | `8` | a single service this surprising opens one alone |
| `engine.incident.confirm_windows` | `3` | consecutive anomalous windows to confirm a series |
| `engine.incident.warmup` | detector calibration (~25 min at 10 s) | no incidents before this much data |
| `engine.incident.entry_points` | auto | services whose traffic changes count as symptoms users see |
| `engine.incident.exclude` | `[]` | services never reported as causes (e.g. load generators) |
| `engine.rca.method` | `etio` | `etio`, `max_score`, `baro`, `nsigma`, `random_walk` |
| `storage.dir` | none (memory only) | snapshots and SQLite; set it in production |
| `storage.snapshot_interval` | `60s` | snapshot period |
| `limits.queue_batches` | `256` | ingest queue; beyond it clients are asked to back off |
| `limits.max_request_bytes` / `max_decompressed_bytes` | 8 MiB / 64 MiB | request and decompression bounds |
| `clock` | `wall` | `event` follows telemetry timestamps (replays, fast simulations) |
| `cluster.*` | standalone | see [Distributed mode](#distributed-mode) |

Unknown keys are errors, so a typo cannot silently fall back to a default.

**Choosing the resolution.** Detection needs a few windows to confirm an
anomaly (`confirm_windows`), so time-to-detect is roughly
`(confirm_windows + 1) × resolution + lateness`: about 60 s at the defaults,
about 20 s at `resolution = 2s`. Smaller windows are noisier on low-traffic
services and cost more memory per unit of retention (`retention /
resolution` points per series).

## Sizing

* **CPU.** Decoding is parallel; the engine thread is the ceiling (see
  `etio sim bench` for your hardware and topology; one engine thread
  sustains more than half a million spans per second). `etio_tick_seconds` shows how
  close the engine is to saturation.
* **Memory** is dominated by the series store: `series × retention /
  resolution × 8` bytes plus detector state, about 10 KiB per series at the
  defaults. A 100-service system with 20 series per service needs roughly
  20–40 MiB. Span buffers are bounded by `engine.limits`.
* **Disk.** Snapshots are about the size of the series store after lz4
  compression; SQLite grows with incidents (a few KiB each).

## Observability of Etio itself

`GET /metrics` (OpenMetrics) exposes, among others:

| metric | watch for |
|---|---|
| `etio_ingest_requests_total{outcome="backpressure"}` | the engine cannot keep up: scale out or raise resolution |
| `etio_queue_depth` | sustained values near `limits.queue_batches` |
| `etio_ingest_items_rejected_total` | malformed telemetry |
| `etio_late_observations` | clocks skewed or pipelines slower than `lateness` |
| `etio_tick_seconds` | window processing time; must stay well below the tick |
| `etio_buffered_spans` | traces that never complete (missing root spans) |
| `etio_cluster_summaries{event=...}` | distributed mode: `late`, `lost`, `gaps` should stay at 0 |
| `etio_webhook_deliveries_total{outcome="failed"}` | notification endpoint problems |

`/healthz` answers as long as the process runs; `/readyz` only when the
engine thread answers within 2 s.

## Persistence and upgrades

With `storage.dir` set, the engine state is snapshotted every
`snapshot_interval` and on shutdown (`SIGTERM`/`SIGINT`: the server stops
accepting requests, drains its queue, writes a final snapshot, then exits).
On start the snapshot is restored; windows whose data was in flight when it
was taken are marked as gaps, so the restart does not cause alarms.
Snapshots carry a format version and the window width: an incompatible
snapshot (new format, changed `engine.resolution`) is ignored with a warning
and the engine starts fresh, relearning its baselines.

Incidents and feedback are also written to SQLite and remain queryable
after they leave the in-memory retention.

## Notifications

```toml
[[notify.webhooks]]
url = "https://hooks.example.com/etio"
events = ["opened", "analyzed", "resolved"]   # default: all
secret_file = "/run/secrets/webhook"          # optional HMAC-SHA256 signature
```

Deliveries are JSON (`{"event": "opened", "incident": {...}, "summary":
[["cart", 0.94], ...]}`, with `X-Etio-Event` and `X-Etio-Incident` headers),
retried with exponential backoff, and signed when a secret is configured:
`X-Etio-Signature: sha256=<hex HMAC of the body>`. Etio can also receive
Alertmanager webhooks (`POST /api/v1/alerts/alertmanager`) to analyse around
an alert raised elsewhere.

## Distributed mode

See [architecture](architecture.md#distributed-mode) for the design. A
working example is in [`examples/cluster`](../examples/cluster).

```toml
# core
[cluster]
role = "core"
listen = "0.0.0.0:7071"
token_file = "/run/secrets/cluster_token"
deadline = "30s"     # event time an edge may lag before windows close without it
liveness = "2m"      # silence after which an edge is no longer waited for

# edge
[cluster]
role = "edge"
edge_id = "edge-eu-1"                       # stable, unique
cores = ["https://etio-core-0:7071", "https://etio-core-1:7071"]
ca_file = "/etc/etio/ca.pem"                # required for https cores
token_file = "/run/secrets/cluster_token"
outbox_capacity = 4320                      # windows retained while cores are away
```

Route traces to edges by trace ID (OpenTelemetry Collector `loadbalancing`
exporter with `routing_key: traceID`); metrics may go anywhere (routing by
service keeps each counter stream on one edge, which avoids double
counting). All nodes must use the same `engine.resolution`; cores refuse
edges that differ.

## Troubleshooting

| symptom | likely cause |
|---|---|
| no incidents at all | still in warm-up (`engine.incident.warmup`), or `min_services` too high for a small system |
| incidents but empty ranking | analysis runs `rca_delay` after opening; check `analyses_skipped` in `/api/v1/status` |
| many `late_observations` | a pipeline stage batches longer than `engine.lateness`, or skewed clocks |
| spans but no graph | spans lack `parent_span_id` links across services (context propagation broken) |
| a datastore never appears | it emits nothing: it is only visible through client spans that name it (`server.address`, `db.system`) |
