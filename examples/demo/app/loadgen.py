"""Load generator and chaos controller for the demo.

* Sends ``RATE`` requests per second to the frontend, like users would
  (it is deliberately not instrumented: users do not send telemetry).
* Serves ``POST /chaos/<service>?latency_ms=..&cpu_factor=..&error_rate=..&duration_s=..``
  on port 8089 and forwards it to that service.
* Runs ``SCHEDULE`` (e.g. ``payment:latency_ms=300@15m+5m;cart:cpu_factor=8@40m+5m``)
  so that the demo produces an incident without any interaction.
"""

from __future__ import annotations

import os
import re
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

import requests

TARGET = os.environ.get("TARGET", "http://frontend:8000/")
RATE = float(os.environ.get("RATE", "20"))
SCHEDULE = os.environ.get("SCHEDULE", "")
SERVICES = set(os.environ.get("SERVICES", "frontend,catalog,cart,checkout,payment").split(","))


def seconds(text: str) -> float:
    m = re.fullmatch(r"(\d+(?:\.\d+)?)(ms|s|m|h)?", text.strip())
    if not m:
        raise ValueError(f"bad duration {text!r}")
    return float(m.group(1)) * {"ms": 0.001, "s": 1, "m": 60, "h": 3600, None: 1}[m.group(2)]


def inject(service: str, query: str) -> int:
    if service not in SERVICES:
        return 404
    try:
        return requests.post(f"http://{service}:8000/chaos?{query}", timeout=5).status_code
    except requests.RequestException:
        return 502


def run_schedule() -> None:
    start = time.time()
    for item in filter(None, (s.strip() for s in SCHEDULE.split(";"))):
        target, _, when = item.partition("@")
        service, _, params = target.partition(":")
        at, _, duration = when.partition("+")
        time.sleep(max(0.0, start + seconds(at) - time.time()))
        query = "&".join(filter(None, [params.replace(",", "&"), f"duration_s={seconds(duration or '5m')}"]))
        print(f"schedule: injecting {query} into {service}: HTTP {inject(service, query)}", flush=True)


class Chaos(BaseHTTPRequestHandler):
    def do_POST(self) -> None:  # noqa: N802 (http.server API)
        parts = urlsplit(self.path)
        m = re.fullmatch(r"/chaos/([a-z0-9-]+)", parts.path)
        code = inject(m.group(1), parts.query) if m else 404
        self.send_response(code)
        self.end_headers()
        self.wfile.write(b"ok\n" if code == 200 else b"failed\n")

    def log_message(self, fmt: str, *args) -> None:
        print(f"chaos: {fmt % args}", flush=True)


def load() -> None:
    session = requests.Session()
    pool = ThreadPoolExecutor(max_workers=64)

    def one() -> None:
        try:
            session.get(TARGET, timeout=10)
        except requests.RequestException:
            pass

    interval = 1.0 / RATE
    next_at = time.perf_counter()
    while True:
        pool.submit(one)
        next_at += interval
        time.sleep(max(0.0, next_at - time.perf_counter()))


if __name__ == "__main__":
    threading.Thread(target=run_schedule, daemon=True).start()
    threading.Thread(target=load, daemon=True).start()
    ThreadingHTTPServer(("0.0.0.0", 8089), Chaos).serve_forever()
