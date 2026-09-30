# Demo: an instrumented shop

A small online shop of five Python services, instrumented the way real
applications are: with the standard OpenTelemetry SDK and zero-code
instrumentation (`opentelemetry-instrument`), no Etio-specific code. They
send traces, metrics and logs to an OpenTelemetry Collector, which forwards
them to Etio.

```
loadgen ─► frontend ─┬─► catalog
                     ├─► cart ─► redis
                     └─► checkout ─┬─► payment
                                   └─► cart
```

```sh
docker compose up -d --build
open http://127.0.0.1:7070
```

Etio learns what normal looks like for 10 minutes (shortened from the
production default for the demo). At minute 15 the load generator makes
`payment` slow for five minutes, and an incident appears with `payment` as
the most likely cause, its latency, local time and inbound latency as
evidence, and its callers (`checkout`, `frontend`) explained away.

Inject your own faults at any time:

```sh
curl -X POST 'http://127.0.0.1:8089/chaos/cart?cpu_factor=8&duration_s=300'    # CPU starvation
curl -X POST 'http://127.0.0.1:8089/chaos/catalog?error_rate=0.3&duration_s=300' # errors
curl -X POST 'http://127.0.0.1:8089/chaos/payment?latency_ms=300&duration_s=300' # slowness
```

or change the schedule: `SCHEDULE='cart:cpu_factor=8@15m+5m' docker compose up -d`.

Ports bind to 127.0.0.1 only; override them with `ETIO_UI_PORT` and
`CHAOS_PORT`. Stop and remove everything with `docker compose down -v`.
