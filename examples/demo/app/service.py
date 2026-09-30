"""One service of the demo shop, configured by environment variables.

Instrumentation is zero-code: the container runs this file under
``opentelemetry-instrument``, which traces Flask, ``requests`` and Redis,
exports logs, and reports process metrics, exactly as an unmodified
application would be instrumented in production.

Environment:
    OTEL_SERVICE_NAME  service name (also the host name of the container)
    CALLS              downstream services called on every request, e.g. "cart,catalog"
    WORK_MS            median CPU work per request, milliseconds
    REDIS_HOST         if set, every request reads and writes a key there
"""

from __future__ import annotations

import logging
import math
import os
import random
import threading
import time

import requests
from flask import Flask, jsonify, request

SERVICE = os.environ.get("OTEL_SERVICE_NAME", "service")
CALLS = [c for c in os.environ.get("CALLS", "").split(",") if c]
WORK_MS = float(os.environ.get("WORK_MS", "3"))
REDIS_HOST = os.environ.get("REDIS_HOST")

app = Flask(SERVICE)
log = logging.getLogger(SERVICE)
logging.basicConfig(level=logging.INFO, format="%(levelname)s %(name)s %(message)s")
session = requests.Session()
cache = None
if REDIS_HOST:
    import redis

    cache = redis.Redis(host=REDIS_HOST, socket_timeout=2)


class Chaos:
    """Faults injected through ``POST /chaos``; they expire on their own."""

    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.latency_ms = 0.0
        self.cpu_factor = 1.0
        self.error_rate = 0.0
        self.until = 0.0

    def set(self, latency_ms: float, cpu_factor: float, error_rate: float, duration_s: float) -> None:
        with self.lock:
            self.latency_ms, self.cpu_factor, self.error_rate = latency_ms, cpu_factor, error_rate
            self.until = time.time() + duration_s

    def current(self) -> tuple[float, float, float]:
        with self.lock:
            if time.time() > self.until:
                return 0.0, 1.0, 0.0
            return self.latency_ms, self.cpu_factor, self.error_rate


chaos = Chaos()


def burn(ms: float) -> None:
    """Busy CPU work (so CPU faults show up as CPU and as self-time)."""
    end = time.perf_counter() + ms / 1000.0
    x = 0.0
    while time.perf_counter() < end:
        x += math.sqrt(random.random())


@app.get("/")
def handle():
    latency_ms, cpu_factor, error_rate = chaos.current()
    burn(random.lognormvariate(math.log(max(WORK_MS, 0.1)), 0.3) * cpu_factor)
    if latency_ms:
        time.sleep(latency_ms / 1000.0)
    if cache is not None:
        key = f"item:{random.randrange(1000)}"
        if cache.get(key) is None:
            cache.set(key, "1", ex=60)
    for callee in CALLS:
        r = session.get(f"http://{callee}:8000/", timeout=5)
        if r.status_code >= 500:
            log.error("call to %s failed with %s", callee, r.status_code)
            return jsonify(error=f"{callee} failed"), 502
    if random.random() < error_rate:
        log.error("InventoryUnavailableException: simulated failure in %s", SERVICE)
        return jsonify(error="internal error"), 500
    return jsonify(service=SERVICE, ok=True)


@app.post("/chaos")
def set_chaos():
    args = request.args
    chaos.set(
        latency_ms=float(args.get("latency_ms", 0)),
        cpu_factor=float(args.get("cpu_factor", 1)),
        error_rate=float(args.get("error_rate", 0)),
        duration_s=float(args.get("duration_s", 300)),
    )
    log.warning("chaos injected: %s", dict(args))
    return jsonify(ok=True)


@app.get("/healthz")
def healthz():
    return "ok"


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=8000, threaded=True)
