"""The native extension: the same Rust code the server runs."""

import json

import numpy as np

import etio


def synthetic(fault: str = "cart"):
    """Two services, one-minute windows; ``fault`` gets slow after t=600 s."""
    times = np.arange(0.0, 900.0, 10.0)
    rng = np.random.default_rng(0)
    services, names, rows = [], [], []
    for svc in ["frontend", "cart", "catalog"]:
        latency = 20 + rng.normal(0, 1, times.size)
        if svc == fault:
            latency[times >= 600] += 40
        services.append(svc)
        names.append("latency_p95")
        rows.append(latency)
    return times, np.vstack(rows), services, names


def test_analyze_ranks_the_faulty_service_first():
    times, values, services, names = synthetic()
    out = json.loads(etio.analyze(times, values, services, names, 600.0))
    assert out["ranking"][0]["service"] == "cart"
    probs = [r["probability"] for r in out["ranking"]]
    assert abs(sum(probs) - 1.0) < 1e-6


def test_every_method_accepts_the_same_input():
    times, values, services, names = synthetic("catalog")
    for method in ["etio", "baro", "nsigma"]:
        cfg = json.dumps({"method": method})
        out = json.loads(etio.analyze(times, values, services, names, 600.0, config=cfg))
        assert out["ranking"][0]["service"] == "catalog", method


def test_features_match_the_model_schema():
    times, values, services, names = synthetic()
    svcs, x = etio.service_features(times, values, services, names, 600.0)
    model = json.loads(etio.heuristic_model())
    assert sorted(svcs) == sorted(services)
    assert model["feature_set"] == 2
    # The model may use a subset of the feature set.
    assert x.shape[0] == len(svcs) and x.shape[1] >= len(model["features"])
    assert np.isfinite(x).all()
