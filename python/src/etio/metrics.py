"""Ranking metrics with bootstrap confidence intervals.

AC@k is the fraction of cases whose true root cause is among the first ``k``
candidates; Avg@k is the mean of AC@1..AC@k, the headline metric of the RCA
literature. Every number reported by the harness comes with a percentile
bootstrap interval over cases: with 30-125 cases per dataset, differences of
a few points between methods are often noise, and the report should say so.
"""

from __future__ import annotations

from collections.abc import Callable, Sequence
from dataclasses import dataclass

import numpy as np

Ranks = Sequence[int | None]  # 1-based rank of the answer per case; None = not ranked


def rank_of(ranking: Sequence[str], answer: str) -> int | None:
    """1-based position of ``answer`` in ``ranking``."""
    for i, candidate in enumerate(ranking):
        if candidate == answer:
            return i + 1
    return None


def dedup(items: Sequence[str]) -> list[str]:
    """Keeps the first occurrence of every item."""
    seen: set[str] = set()
    out = []
    for x in items:
        if x not in seen:
            seen.add(x)
            out.append(x)
    return out


def _hits(ranks: Ranks, k: int) -> np.ndarray:
    return np.array([r is not None and r <= k for r in ranks], dtype=np.float64)


def accuracy(ranks: Ranks, k: int) -> float:
    """AC@k."""
    return float(_hits(ranks, k).mean()) if len(ranks) else float("nan")


def average(ranks: Ranks, k: int = 5) -> float:
    """Avg@k = mean of AC@1..AC@k."""
    return float(np.mean([accuracy(ranks, j) for j in range(1, k + 1)])) if len(ranks) else float("nan")


def mrr(ranks: Ranks) -> float:
    """Mean reciprocal rank (0 for unranked answers)."""
    return float(np.mean([1.0 / r if r else 0.0 for r in ranks])) if len(ranks) else float("nan")


@dataclass(frozen=True)
class Estimate:
    """A point estimate with a bootstrap interval."""

    value: float
    low: float
    high: float

    def fmt(self, digits: int = 2) -> str:
        return f"{self.value:.{digits}f} [{self.low:.{digits}f}, {self.high:.{digits}f}]"


def bootstrap(
    ranks: Ranks,
    stat: Callable[[Ranks], float],
    *,
    n_boot: int = 2000,
    alpha: float = 0.05,
    seed: int = 0,
) -> Estimate:
    """Percentile bootstrap interval of ``stat`` over cases."""
    ranks = list(ranks)
    if not ranks:
        nan = float("nan")
        return Estimate(nan, nan, nan)
    rng = np.random.default_rng(seed)
    n = len(ranks)
    samples = np.empty(n_boot)
    for b in range(n_boot):
        idx = rng.integers(0, n, n)
        samples[b] = stat([ranks[i] for i in idx])
    lo, hi = np.quantile(samples, [alpha / 2, 1 - alpha / 2])
    return Estimate(stat(ranks), float(lo), float(hi))


def summarize(ranks: Ranks, *, n_boot: int = 2000, seed: int = 0) -> dict[str, Estimate]:
    """AC@1, AC@3, AC@5, Avg@5 and MRR with 95% intervals."""
    return {
        "AC@1": bootstrap(ranks, lambda r: accuracy(r, 1), n_boot=n_boot, seed=seed),
        "AC@3": bootstrap(ranks, lambda r: accuracy(r, 3), n_boot=n_boot, seed=seed),
        "AC@5": bootstrap(ranks, lambda r: accuracy(r, 5), n_boot=n_boot, seed=seed),
        "Avg@5": bootstrap(ranks, lambda r: average(r, 5), n_boot=n_boot, seed=seed),
        "MRR": bootstrap(ranks, mrr, n_boot=n_boot, seed=seed),
    }


def paired_difference(a: Ranks, b: Ranks, *, n_boot: int = 4000, seed: int = 0) -> Estimate:
    """Bootstrap interval of Avg@5(a) - Avg@5(b) over the same cases (paired)."""
    a, b = list(a), list(b)
    if len(a) != len(b):
        raise ValueError("paired comparison needs the same cases")
    rng = np.random.default_rng(seed)
    n = len(a)
    diffs = np.empty(n_boot)
    for i in range(n_boot):
        idx = rng.integers(0, n, n)
        diffs[i] = average([a[j] for j in idx]) - average([b[j] for j in idx])
    lo, hi = np.quantile(diffs, [0.025, 0.975])
    return Estimate(average(a) - average(b), float(lo), float(hi))
