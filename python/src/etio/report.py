"""Turns per-case results into summary tables."""

from __future__ import annotations

from collections import defaultdict
from collections.abc import Sequence

from etio.benchmark import CaseResult
from etio.metrics import paired_difference, summarize

METRICS = ("AC@1", "AC@3", "AC@5", "Avg@5")


def group(results: Sequence[CaseResult], key: str = "dataset") -> dict[tuple[str, str], list[CaseResult]]:
    groups: dict[tuple[str, str], list[CaseResult]] = defaultdict(list)
    for r in results:
        groups[(getattr(r, key), r.method)].append(r)
        suite = r.dataset.split("-")[0]
        if key == "dataset":
            groups[(suite, r.method)].append(r)
    return groups


def table(
    results: Sequence[CaseResult],
    *,
    methods: Sequence[str] | None = None,
    published: bool = False,
    n_boot: int = 1000,
) -> str:
    """Markdown table: one row per (dataset, method) with 95% bootstrap intervals.

    With ``published=True`` the RCAEval convention (services of the top-k
    metrics, duplicates included) is used where available.
    """
    groups = group(results)
    methods = list(methods) if methods else sorted({r.method for r in results})
    datasets = sorted({d for d, _ in groups}, key=lambda d: (d.split("-")[0], len(d), d))
    lines = [
        "| dataset | method | n | " + " | ".join(METRICS) + " | ms/case |",
        "|---|---|---:|" + "---:|" * len(METRICS) + "---:|",
    ]
    for d in datasets:
        for m in methods:
            rs = groups.get((d, m))
            if not rs:
                continue
            ok = [r for r in rs if r.error is None]
            ranks = [(r.rank_published if published and r.rank_published is not None else r.rank) for r in ok]
            s = summarize(ranks, n_boot=n_boot)
            ms = sum(r.runtime_ms for r in ok) / max(len(ok), 1)
            errors = len(rs) - len(ok)
            n = f"{len(ok)}" + (f" (+{errors} err)" if errors else "")
            cells = " | ".join(s[k].fmt(2) for k in METRICS)
            lines.append(f"| {d} | {m} | {n} | {cells} | {ms:.1f} |")
    return "\n".join(lines)


def comparison(results: Sequence[CaseResult], a: str, b: str) -> str:
    """Paired Avg@5 difference between two methods, per dataset."""
    by_case: dict[tuple[str, str], CaseResult] = {(r.case, r.method): r for r in results if r.error is None}
    datasets = sorted({r.dataset for r in results})
    suites = sorted({d.split("-")[0] for d in datasets})
    lines = [f"| dataset | Avg@5({a}) - Avg@5({b}) | 95% CI |", "|---|---:|---|"]
    for d in datasets + suites:
        cases = sorted({r.case for r in results if (r.dataset == d or r.dataset.startswith(d + "-"))})
        pairs = [(by_case.get((c, a)), by_case.get((c, b))) for c in cases]
        pairs = [(x, y) for x, y in pairs if x is not None and y is not None]
        if not pairs:
            continue
        est = paired_difference([x.rank for x, _ in pairs], [y.rank for _, y in pairs])
        lines.append(f"| {d} | {est.value:+.3f} | [{est.low:+.3f}, {est.high:+.3f}] |")
    return "\n".join(lines)
