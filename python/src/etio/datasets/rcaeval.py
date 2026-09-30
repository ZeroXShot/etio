"""Adapter for the RCAEval benchmark (Pham et al., WWW 2025).

RCAEval contains 735 failure cases injected into three microservice systems
(Online Boutique, Sock Shop, Train Ticket), with the ground-truth root-cause
service of each case. The data is published under the MIT licence on the
Hugging Face Hub; this module downloads individual cases on demand from a
*pinned revision* and verifies every file against the hash recorded by the
Hub (SHA-256 for LFS objects, the git blob SHA-1 for small files), so a
benchmark run always sees exactly the same bytes.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Iterable, Sequence
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import pandas as pd

REPO = "phamquiluan/RCAEval"
#: Dataset revision the published results were computed on.
REVISION = "afeacb11bcc94dadfd1c8f483ee4377b2b8b614e"
HUB = "https://huggingface.co"

SUITES = ("RE1", "RE2", "RE3")
SYSTEMS = {"OB": "Online Boutique", "SS": "Sock Shop", "TT": "Train Ticket"}

#: Cases whose recorded data cannot support an evaluation. They are excluded
#: by default (``RcaEval.cases(include_defective=True)`` keeps them):
#: * the injection timestamp is truncated (``16933142`` instead of ``1693314xxx``);
#: * the metrics stop before the injection, so there is no abnormal period.
DEFECTIVE = {
    "re1ob_currencyservice_loss_1": "truncated inject_time",
    "re1ob_productcatalogservice_cpu_3": "no data after the injection",
}

#: Service names that differ between telemetry sources, per system.
ALIASES = {"OB": {"frontendservice": "frontend"}, "SS": {}, "TT": {}}

#: Entry-point services of each system (they receive user traffic).
ENTRY_POINTS = {"OB": ["frontend"], "SS": ["front-end"], "TT": ["ts-ui-dashboard"]}


class DatasetError(RuntimeError):
    """Raised when a case cannot be downloaded or fails verification."""


@dataclass(frozen=True)
class Case:
    """One failure case of the benchmark."""

    name: str
    dataset: str  # e.g. "RE1-OB"
    suite: str  # "RE1", "RE2" or "RE3"
    system: str  # "OB", "SS" or "TT"
    root_cause: str
    fault: str
    repetition: int
    inject_time: float
    has_logs: bool
    has_traces: bool


def default_root() -> Path:
    """Data directory: ``$ETIO_DATA_DIR/rcaeval`` or ``./data/rcaeval``."""
    base = os.environ.get("ETIO_DATA_DIR")
    return (Path(base) if base else Path.cwd() / "data") / "rcaeval"


def _git_blob_sha1(data: bytes) -> str:
    return hashlib.sha1(b"blob %d\0" % len(data) + data, usedforsecurity=False).hexdigest()


def _get(url: str, *, data: bytes | None = None, attempts: int = 4, timeout: float = 60.0) -> bytes:
    last: Exception | None = None
    for attempt in range(attempts):
        try:
            req = urllib.request.Request(url, data=data, headers={"User-Agent": "etio-eval"})
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                return resp.read()
        except (urllib.error.URLError, TimeoutError, ConnectionError) as e:
            last = e
            time.sleep(min(2.0**attempt, 10.0))
    raise DatasetError(f"failed to fetch {url}: {last}")


class RcaEval:
    """Lazily downloaded, integrity-checked local copy of RCAEval."""

    def __init__(self, root: Path | str | None = None, revision: str = REVISION) -> None:
        self.revision = revision
        self.root = Path(root) if root is not None else default_root()
        self.dir = self.root / revision[:12]
        self._index: pd.DataFrame | None = None
        self._manifest: dict | None = None
        self._lock = threading.Lock()

    # -- low level -----------------------------------------------------------------

    def _manifest_path(self) -> Path:
        return self.dir / "manifest.json"

    def _metadata(self, paths: Sequence[str]) -> dict[str, dict]:
        """Hub metadata (size and hashes) of files at the pinned revision.

        Fetched in batches through the ``paths-info`` endpoint and cached in a
        local manifest, so an offline re-run needs no network access.
        """
        with self._lock:
            manifest = self._manifest
            if manifest is None:
                mp = self._manifest_path()
                manifest = json.loads(mp.read_text()) if mp.exists() else {}
                self._manifest = manifest
            missing = [p for p in paths if p not in manifest]
            for i in range(0, len(missing), 100):
                chunk = missing[i : i + 100]
                body = urllib.parse.urlencode([("paths", p) for p in chunk] + [("expand", "true")])
                url = f"{HUB}/api/datasets/{REPO}/paths-info/{self.revision}"
                for entry in json.loads(_get(url, data=body.encode())):
                    manifest[entry["path"]] = {
                        "size": entry["size"],
                        "oid": entry["oid"],
                        "lfs": (entry.get("lfs") or {}).get("oid"),
                    }
            if missing:
                self.dir.mkdir(parents=True, exist_ok=True)
                tmp = self._manifest_path().with_suffix(".tmp")
                tmp.write_text(json.dumps(manifest, indent=0, sort_keys=True))
                tmp.replace(self._manifest_path())
            return {p: manifest[p] for p in paths if p in manifest}

    @staticmethod
    def _verify(path: str, data: bytes, meta: dict) -> None:
        if len(data) != meta["size"]:
            raise DatasetError(f"{path}: size {len(data)} != {meta['size']}")
        if meta.get("lfs"):
            if hashlib.sha256(data).hexdigest() != meta["lfs"]:
                raise DatasetError(f"{path}: sha256 mismatch")
        elif _git_blob_sha1(data) != meta["oid"]:
            raise DatasetError(f"{path}: git blob hash mismatch")

    def fetch(self, path: str) -> Path:
        """Downloads one file (if missing), verifies it, and returns its local path."""
        local = self.dir / path
        if local.exists():
            return local
        meta = self._metadata([path]).get(path)
        if meta is None:
            raise DatasetError(f"{path} is not part of revision {self.revision}")
        data = _get(f"{HUB}/datasets/{REPO}/resolve/{self.revision}/{path}")
        self._verify(path, data, meta)
        local.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(dir=local.parent, delete=False) as tmp:
            tmp.write(data)
        shutil.move(tmp.name, local)
        return local

    def prefetch(self, paths: Sequence[str]) -> None:
        """Resolves metadata for many files in batches, then downloads them."""
        self._metadata([p for p in paths if not (self.dir / p).exists()])
        for p in paths:
            self.fetch(p)

    # -- index -----------------------------------------------------------------------

    def index(self) -> pd.DataFrame:
        """The case index (735 rows)."""
        if self._index is None:
            self._index = pd.read_parquet(self.fetch("cases.parquet"))
        return self._index

    def cases(self, datasets: Iterable[str] | None = None, *, include_defective: bool = False) -> list[Case]:
        """Cases of the given datasets (e.g. ``["RE1-OB", "RE2"]``), in index order."""
        idx = self.index()
        if datasets is not None:
            wanted = [d.upper() for d in datasets]
            mask = idx["dataset"].isin(wanted) | idx["suite"].isin(wanted)
            idx = idx[mask]
        if not include_defective:
            idx = idx[~idx["case"].isin(DEFECTIVE)]
        return [
            Case(
                name=r.case,
                dataset=r.dataset,
                suite=r.suite,
                system=r.dataset.split("-")[1],
                root_cause=r.root_cause_service,
                fault=r.fault,
                repetition=int(r.repetition),
                inject_time=float(r.inject_time),
                has_logs=bool(r.has_logs),
                has_traces=bool(r.has_traces),
            )
            for r in idx.itertuples()
        ]

    # -- telemetry -------------------------------------------------------------------

    def metrics(self, case: Case | str) -> pd.DataFrame:
        name = case.name if isinstance(case, Case) else case
        return pd.read_parquet(self.fetch(f"{name}/metrics.parquet"))

    def logs(self, case: Case | str) -> pd.DataFrame | None:
        c = case if isinstance(case, Case) else self._case(case)
        if not c.has_logs:
            return None
        return pd.read_parquet(self.fetch(f"{c.name}/logs.parquet"))

    def traces(self, case: Case | str) -> pd.DataFrame | None:
        c = case if isinstance(case, Case) else self._case(case)
        if not c.has_traces:
            return None
        return pd.read_parquet(self.fetch(f"{c.name}/traces.parquet"))

    def _case(self, name: str) -> Case:
        for c in self.cases():
            if c.name == name:
                return c
        raise DatasetError(f"unknown case {name}")


@dataclass
class MetricMatrix:
    """Metrics of a case in the layout the native analysis expects."""

    times: np.ndarray  # (T,) seconds since the epoch
    values: np.ndarray  # (S, T) one row per series
    services: list[str]
    names: list[str]
    columns: list[str]


def split_column(column: str) -> tuple[str, str]:
    """``"ts-auth-service_latency-90"`` -> ``("ts-auth-service", "latency-90")``."""
    service, _, name = column.partition("_")
    return service, (name or column)


def metric_matrix(df: pd.DataFrame, drop: Sequence[str] = ("time", "time.1")) -> MetricMatrix:
    """Converts an RCAEval metrics frame into aligned arrays.

    Memory columns are converted to megabytes, as RCAEval's own preprocessing
    does; Etio's scoring is unit-free, but faithful baselines are not.
    """
    df = df.sort_values("time").drop_duplicates("time", keep="last")
    cols = [c for c in df.columns if c not in drop]
    values = df[cols].to_numpy(dtype=np.float64, na_value=np.nan).T.copy()
    for i, c in enumerate(cols):
        if c.endswith("_mem"):
            values[i] /= 1e6
    services, names = zip(*(split_column(c) for c in cols), strict=True) if cols else ((), ())
    return MetricMatrix(
        times=df["time"].to_numpy(dtype=np.float64),
        values=np.ascontiguousarray(values),
        services=list(services),
        names=list(names),
        columns=cols,
    )


@dataclass
class CaseInput:
    """All telemetry of a case aligned on one grid, ready for ``etio.analyze``."""

    times: np.ndarray
    values: np.ndarray
    services: list[str]
    names: list[str]
    categories: list[str]
    edges: list[tuple[str, str, float]]
    sources: tuple[str, ...]
    stats: dict


def case_input(ds: RcaEval, case: Case, sources: Sequence[str] = ("metrics",)) -> CaseInput:
    """Builds the analysis input of a case from the requested telemetry sources.

    Traces and logs are converted by the native pipeline on the metrics' 1 s
    grid. Sources a case does not have are skipped (Sock Shop has no traces,
    some cases have no logs); ``CaseInput.sources`` lists what was used.
    """
    import etio  # noqa: PLC0415 - avoid a cycle at import time

    mm = metric_matrix(ds.metrics(case))
    start, end = float(mm.times[0]), float(mm.times[-1])
    blocks = [(mm.values, mm.services, mm.names, [etio.classify_metric(n) for n in mm.names])]
    edges: list[tuple[str, str, float]] = []
    used = ["metrics"]
    stats: dict = {}
    alias = ALIASES.get(case.system, {})

    def rename(names: Sequence[str]) -> list[str]:
        return [alias.get(n, n) for n in names]

    def aligned(times: np.ndarray, values: np.ndarray) -> np.ndarray:
        # The converters use the same grid; guard against off-by-one ends.
        out = np.full((values.shape[0], len(mm.times)), np.nan)
        n = min(len(times), len(mm.times))
        out[:, :n] = values[:, :n]
        return out

    if "traces" in sources and case.has_traces:
        t = ds.traces(case)
        if t is not None and len(t):
            status = t["statusCode"].fillna(0).to_numpy(dtype=np.int64)
            parents = t["parentSpanID"].astype(object).where(t["parentSpanID"].notna(), None).tolist()
            times, values, svc, names, cats, e, st = etio.trace_series(
                t["traceID"].astype(str).tolist(),
                t["spanID"].astype(str).tolist(),
                parents,
                rename(t["serviceName"].fillna("unknown").astype(str).tolist()),
                t["operationName"].fillna("").astype(str).tolist(),
                t["startTime"].fillna(0).to_numpy(dtype=np.int64),
                t["duration"].fillna(0).to_numpy(dtype=np.int64),
                status > 0,
                start_s=start,
                end_s=end,
            )
            blocks.append((aligned(times, values), svc, names, cats))
            edges = [(a, b, w) for a, b, w in e if a != b]
            stats["traces"] = json.loads(st)
            used.append("traces")

    if "logs" in sources and case.has_logs:
        try:
            lg = ds.logs(case)
        except DatasetError:
            lg = None  # a few cases announce logs that were never published
        if lg is not None and len(lg):
            times, values, svc, names, cats, _, st = etio.log_series(
                lg["timestamp"].to_numpy(dtype=np.float64),
                rename(lg["container_name"].fillna("unknown").astype(str).tolist()),
                lg["message"].fillna("").astype(str).tolist(),
                start_s=start,
                end_s=end,
                novelty_after_s=case.inject_time,
            )
            blocks.append((aligned(times, values), svc, names, cats))
            stats["logs"] = json.loads(st)
            used.append("logs")

    return CaseInput(
        times=mm.times,
        values=np.ascontiguousarray(np.vstack([b[0] for b in blocks])),
        services=[x for b in blocks for x in b[1]],
        names=[x for b in blocks for x in b[2]],
        categories=[x for b in blocks for x in b[3]],
        edges=edges,
        sources=tuple(used),
        stats=stats,
    )
