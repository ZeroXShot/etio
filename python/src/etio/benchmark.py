"""Runs ranking methods over benchmark cases and collects per-case results."""

from __future__ import annotations

import json
import platform
import sys
import time
import traceback
from collections.abc import Callable, Iterable, Sequence
from concurrent.futures import ThreadPoolExecutor
from dataclasses import asdict, dataclass, field
from pathlib import Path

import pandas as pd

import etio
from etio.datasets.rcaeval import REVISION, Case, CaseInput, RcaEval, case_input, split_column
from etio.metrics import dedup, rank_of

#: Official implementations from the RCAEval package, keyed by our name.
OFFICIAL = {"rcaeval_baro": "baro", "rcaeval_nsigma": "nsigma"}


@dataclass
class CaseResult:
    """Outcome of one method on one case."""

    case: str
    dataset: str
    system: str
    fault: str
    root_cause: str
    method: str
    ranking: list[str]
    rank: int | None
    #: Rank under RCAEval's published convention: the services of the top-k
    #: *metrics*, duplicates included. Only defined for metric-level methods.
    rank_published: int | None = None
    n_candidates: int = 0
    runtime_ms: float = 0.0
    error: str | None = None
    top_probability: float | None = None
    extra: dict = field(default_factory=dict)


def _result(case: Case, method: str, ranking: Sequence[str], started: float, **kw) -> CaseResult:
    return CaseResult(
        case=case.name,
        dataset=case.dataset,
        system=case.system,
        fault=case.fault,
        root_cause=case.root_cause,
        method=method,
        ranking=list(ranking[:10]),
        rank=rank_of(ranking, case.root_cause),
        n_candidates=len(ranking),
        runtime_ms=(time.perf_counter() - started) * 1e3,
        **kw,
    )


def run_native(
    case: Case,
    data: CaseInput,
    method: str,
    *,
    config: dict | None = None,
    edges: list[tuple[str, str, float]] | None = None,
    exclude: list[str] | None = None,
) -> CaseResult:
    """Runs one of Etio's native methods on prepared case data."""
    cfg = dict(config or {})
    cfg["method"] = method
    started = time.perf_counter()
    out = json.loads(
        etio.analyze(
            data.times,
            data.values,
            data.services,
            data.names,
            case.inject_time,
            categories=data.categories,
            edges=edges if edges is not None else (data.edges or None),
            exclude=exclude,
            config=json.dumps(cfg),
        )
    )
    ranking = [r["service"] for r in out["ranking"]]
    top_p = out["ranking"][0]["probability"] if out["ranking"] else None
    return _result(case, method, ranking, started, top_probability=top_p, extra={"sources": list(data.sources)})


def run_official(case: Case, df: pd.DataFrame, method: str) -> CaseResult:
    """Runs an official RCAEval implementation (needs ``etio[baselines]``)."""
    from RCAEval import e2e  # noqa: PLC0415 - optional dependency

    fn = getattr(e2e, OFFICIAL[method])
    started = time.perf_counter()
    out = fn(df.copy(), inject_time=case.inject_time, dataset=case.dataset.lower())
    metric_ranks: list[str] = list(out["ranks"])
    services_dup = [split_column(m)[0] for m in metric_ranks]
    res = _result(case, method, dedup(services_dup), started)
    res.rank_published = rank_of(services_dup, case.root_cause)
    res.extra = {"top_metrics": metric_ranks[:5]}
    return res


def environment() -> dict:
    """Provenance of a run. Deliberately excludes host names and paths."""
    return {
        "etio_version": etio.__version__,
        "dataset": "RCAEval",
        "dataset_revision": REVISION,
        "python": sys.version.split()[0],
        "machine": platform.machine(),
        "system": platform.system(),
    }


def run(
    ds: RcaEval,
    cases: Iterable[Case],
    methods: Sequence[str],
    *,
    config: dict | None = None,
    graphs: dict[str, list[tuple[str, str, float]]] | None = None,
    exclude: dict[str, list[str]] | None = None,
    workers: int = 2,
    sources: Sequence[str] = ("metrics",),
    progress: Callable[[int, int, str], None] | None = None,
) -> list[CaseResult]:
    """Runs every method on every case.

    ``graphs`` and ``exclude`` are keyed by system (``"OB"``, ``"SS"``, ``"TT"``).
    Cases whose data is unusable (for example an empty reference period) are
    reported with an ``error`` instead of aborting the run.
    """
    cases = list(cases)
    if any(m in OFFICIAL for m in methods):
        # Import once, before the worker threads start: concurrent first
        # imports of the same package race on partially initialised modules.
        import RCAEval.e2e  # noqa: F401, PLC0415 - optional dependency

    def one(case: Case) -> list[CaseResult]:
        df = ds.metrics(case)
        data = case_input(ds, case, sources) if any(m not in OFFICIAL for m in methods) else None
        results = []
        for m in methods:
            try:
                if m in OFFICIAL:
                    results.append(run_official(case, df, m))
                else:
                    results.append(
                        run_native(
                            case,
                            data,
                            m,
                            config=config,
                            edges=(graphs or {}).get(case.system),
                            exclude=(exclude or {}).get(case.system),
                        )
                    )
            except Exception as e:
                results.append(
                    CaseResult(
                        case=case.name,
                        dataset=case.dataset,
                        system=case.system,
                        fault=case.fault,
                        root_cause=case.root_cause,
                        method=m,
                        ranking=[],
                        rank=None,
                        error=f"{type(e).__name__}: {e}",
                        extra={"trace": traceback.format_exc(limit=3)},
                    )
                )
        return results

    out: list[CaseResult] = []
    with ThreadPoolExecutor(max_workers=max(1, workers)) as pool:
        for i, res in enumerate(pool.map(one, cases)):
            out.extend(res)
            if progress:
                progress(i + 1, len(cases), cases[i].name)
    return out


def save(results: Sequence[CaseResult], path: Path, meta: dict | None = None) -> None:
    """Writes results and provenance as one JSON document."""
    path.parent.mkdir(parents=True, exist_ok=True)
    doc = {"environment": environment(), "meta": meta or {}, "results": [asdict(r) for r in results]}
    path.write_text(json.dumps(doc, indent=1))


def load(path: Path) -> tuple[dict, list[CaseResult]]:
    doc = json.loads(Path(path).read_text())
    return doc, [CaseResult(**r) for r in doc["results"]]
