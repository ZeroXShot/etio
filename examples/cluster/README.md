# Distributed mode: two edges and a core

```
simulator ─► OTel Collector ──(load balancing by trace ID)──► etio-edge-0 ─┐
                                                          └──► etio-edge-1 ─┴─► etio-core (UI, API)
```

The edges assemble traces and aggregate windows; the core merges the
edges' window summaries and runs detection and analysis. A simulated shop
provides the traffic, with a CPU fault on `cart` after 15 minutes.

```sh
openssl rand -hex 32 > cluster_token && chmod 644 cluster_token   # shared edge-core token
docker compose up -d --build
open http://127.0.0.1:7070
```

The token file is mounted into the containers, which run as an unprivileged
user; it must be readable by them (hence `chmod 644`; in Kubernetes or
Swarm, use a proper secret).

Things to try:

* `docker compose stop etio-core`, wait a minute, `docker compose start
  etio-core`: the edges buffer their summaries and deliver them when the core
  is back (`etio_cluster_summaries` in the core's `/metrics`).
* `docker compose stop etio-edge-1`: after the configured deadline the core
  stops waiting for it and keeps analysing the traffic of `etio-edge-0`.

See [architecture](../../docs/architecture.md#distributed-mode) for the
protocol and its failure semantics.
