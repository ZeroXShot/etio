"""Benchmark metrics."""

import math

from etio import metrics


def test_rank_and_accuracy():
    assert metrics.rank_of(["a", "b", "c"], "b") == 2
    assert metrics.rank_of(["a"], "z") is None
    ranks = [1, 2, None, 5]
    assert metrics.accuracy(ranks, 1) == 0.25
    assert metrics.accuracy(ranks, 5) == 0.75
    # Avg@5 = mean(AC@1..AC@5) = mean(.25, .5, .5, .5, .75)
    assert math.isclose(metrics.average(ranks, 5), 0.5)
    assert math.isclose(metrics.mrr(ranks), (1 + 0.5 + 0 + 0.2) / 4)
    assert math.isnan(metrics.average([], 5))


def test_dedup_keeps_first_occurrence():
    assert metrics.dedup(["b", "a", "b", "c", "a"]) == ["b", "a", "c"]


def test_bootstrap_interval_contains_the_estimate():
    ranks = [1] * 70 + [3] * 20 + [None] * 10
    est = metrics.summarize(ranks, n_boot=500, seed=1)["Avg@5"]
    assert est.low <= est.value <= est.high
