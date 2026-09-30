"""Training the listwise ranking model.

Each benchmark case is a *group*: the candidate services of the incident, one
feature vector each (computed by the native code, so training and inference
share one implementation), and the index of the true root cause. The model is
linear, ``score = w · standardise(x)``, and is fitted by minimising the
listwise cross-entropy of the true root cause under a softmax over the group
(ListNet top-one) plus an L2 penalty. The objective is convex, so Newton's
method with a backtracking line search converges in a handful of iterations
to the unique optimum: training is exact and deterministic.

Evaluation uses nested *leave-one-system-out* cross-validation: the model is
always scored on a microservice system it has never seen, and the L2 strength
is chosen by an inner leave-one-system-out loop on the training systems only.
This is the realistic setting: a user deploys Etio on their own system,
without labelled incidents from it.
"""

from __future__ import annotations

import json
from collections.abc import Callable, Sequence
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field

import numpy as np

import etio
from etio.datasets.rcaeval import REVISION, Case, RcaEval, case_input
from etio.metrics import average, rank_of

FEATURES = etio.feature_names()
L2_GRID = (0.001, 0.01, 0.1, 1.0, 10.0)

#: Features the learned model may not use. ``in_graph`` says whether a service
#: emits traces, i.e. how it is instrumented, not whether it is faulty; in the
#: benchmark it correlates with the answer only because some services lack
#: tracing, and a model that learnt it would not transfer.
EXCLUDED_FEATURES = ("in_graph",)


@dataclass
class Group:
    """Candidates of one incident."""

    case: str
    dataset: str
    system: str
    suite: str
    services: list[str]
    x: np.ndarray  # (n_candidates, n_features)
    y: int  # index of the root cause in `services`, -1 if absent
    root_cause: str


def _cache_key(cases: Sequence[Case], config: dict | None, graphs: dict | None, sources: Sequence[str]) -> str:
    """Identifies a feature extraction: inputs plus the exact native library."""
    import hashlib  # noqa: PLC0415

    native = etio._native.__file__  # type: ignore[attr-defined]
    with open(native, "rb") as f:
        native_digest = hashlib.sha256(f.read()).hexdigest()
    material = json.dumps(
        {
            "cases": [c.name for c in cases],
            "config": config,
            "graphs": graphs,
            "sources": list(sources),
            "native": native_digest,
            "revision": REVISION,
        },
        sort_keys=True,
    )
    return hashlib.sha256(material.encode()).hexdigest()[:16]


def _save_groups(path, groups: Sequence[Group]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fields = ("case", "dataset", "system", "suite", "services", "y", "root_cause")
    doc = [{**{k: getattr(g, k) for k in fields}, "x": g.x.tolist()} for g in groups]
    path.write_text(json.dumps(doc))


def _load_groups(path) -> list[Group]:
    return [Group(**{**d, "x": np.asarray(d["x"], dtype=np.float64)}) for d in json.loads(path.read_text())]


def build_dataset(
    ds: RcaEval,
    cases: Sequence[Case],
    *,
    config: dict | None = None,
    graphs: dict | None = None,
    sources: Sequence[str] = ("metrics",),
    workers: int = 2,
    progress: Callable[[int, int, str], None] | None = None,
    cache_dir=None,
) -> list[Group]:
    """Computes candidate features for every case (skipping unusable cases).

    With ``cache_dir``, results are cached under a key that covers the cases,
    the configuration, the sources and the native library itself.
    """
    from pathlib import Path  # noqa: PLC0415

    cache = Path(cache_dir) / f"features-{_cache_key(cases, config, graphs, sources)}.json" if cache_dir else None
    if cache is not None and cache.exists():
        return _load_groups(cache)
    cfg = json.dumps(config) if config else None

    def one(case: Case) -> Group | None:
        data = case_input(ds, case, sources)
        edges = (graphs or {}).get(case.system) or (data.edges or None)
        try:
            services, x = etio.service_features(
                data.times,
                data.values,
                data.services,
                data.names,
                case.inject_time,
                categories=data.categories,
                edges=edges,
                config=cfg,
            )
        except ValueError:
            return None  # e.g. no reference period before the injection
        y = services.index(case.root_cause) if case.root_cause in services else -1
        return Group(case.name, case.dataset, case.system, case.suite, services, np.asarray(x), y, case.root_cause)

    groups: list[Group] = []
    with ThreadPoolExecutor(max_workers=max(1, workers)) as pool:
        for i, g in enumerate(pool.map(one, cases)):
            if g is not None:
                groups.append(g)
            if progress:
                progress(i + 1, len(cases), cases[i].name)
    if cache is not None:
        _save_groups(cache, groups)
    return groups


# -- model ------------------------------------------------------------------------------


@dataclass
class Linear:
    """A fitted linear scorer over standardised features."""

    weights: np.ndarray
    mean: np.ndarray
    scale: np.ndarray

    def logits(self, x: np.ndarray) -> np.ndarray:
        return ((x - self.mean) / self.scale) @ self.weights

    def rank(self, g: Group) -> int | None:
        return rank_by_scores(g, self.logits(g.x))


def _standardise_scores(v: np.ndarray) -> np.ndarray:
    s = v.std()
    return (v - v.mean()) / (s if s > 1e-12 else 1.0)


def ensemble_scores(*args) -> np.ndarray:
    """Sum of per-incident standardised logits of several models (a product
    of experts). ``ensemble_scores(m1, m2, ..., group)``; the engine computes
    the same combination in ``rca::analyze``."""
    *models, g = args
    return sum(_standardise_scores(m.logits(g.x)) for m in models)


def rank_by_scores(g: Group, scores: np.ndarray) -> int | None:
    """Rank of the root cause, ordering by score then name (as the engine does)."""
    order = sorted(range(len(g.services)), key=lambda i: (-scores[i], g.services[i]))
    return rank_of([g.services[i] for i in order], g.root_cause)


def _standardiser(groups: Sequence[Group]) -> tuple[np.ndarray, np.ndarray]:
    x = np.vstack([g.x for g in groups])
    mean = x.mean(axis=0)
    scale = x.std(axis=0)
    scale[scale < 1e-9] = 1.0
    return mean, scale


def _objective(w: np.ndarray, data: list[tuple[np.ndarray, int]], l2: float, mu: np.ndarray):
    n = len(data)
    loss = 0.0
    grad = np.zeros_like(w)
    hess = np.zeros((w.size, w.size))
    for z, y in data:
        s = z @ w
        m = s.max()
        e = np.exp(s - m)
        p = e / e.sum()
        loss += (m + np.log(e.sum())) - s[y]
        grad += z.T @ p - z[y]
        zp = z.T @ p
        hess += (z.T * p) @ z - np.outer(zp, zp)
    d = w - mu
    loss = loss / n + l2 * (d @ d)
    grad = grad / n + 2 * l2 * d
    hess = hess / n + 2 * l2 * np.eye(w.size)
    return loss, grad, hess


def fit(
    groups: Sequence[Group], l2: float, *, prior: Linear | None = None, max_iter: int = 100, tol: float = 1e-9
) -> Linear:
    """Fits the listwise model by damped Newton iterations.

    With a ``prior``, the penalty is ``l2 * ||w - w_prior||^2`` in standardised
    space: a maximum-a-posteriori estimate under a Gaussian prior centred on
    the prior model instead of on zero. A large ``l2`` returns the prior; a
    small one lets the data decide. Nested cross-validation picks ``l2``, so
    the data move a weight away from the prior only when that helps on
    systems the model has not seen.
    """
    train = [g for g in groups if g.y >= 0]
    if not train:
        raise ValueError("no trainable groups (root cause missing from every candidate set)")
    mean, scale = _standardiser(train)
    # Excluded features are standardised to zero, so their weights stay at zero.
    for name in EXCLUDED_FEATURES:
        k = FEATURES.index(name)
        mean[k], scale[k] = 0.0, 1.0
    mask = np.array([0.0 if f in EXCLUDED_FEATURES else 1.0 for f in FEATURES])
    data = [(((g.x - mean) / scale) * mask, g.y) for g in train]
    # Prior weights expressed on the standardised features: w·(x - m)/s ≡ (w/s)·x + c,
    # so a raw-space weight w_p becomes w_p * s (offsets do not matter under a softmax).
    mu = (prior.weights / prior.scale * scale * mask) if prior is not None else np.zeros(len(FEATURES))
    w = mu.copy()
    loss, grad, hess = _objective(w, data, l2, mu)
    for _ in range(max_iter):
        if np.linalg.norm(grad) < tol:
            break
        step = -np.linalg.solve(hess, grad)
        t = 1.0
        while True:
            cand = w + t * step
            new_loss, new_grad, new_hess = _objective(cand, data, l2, mu)
            if new_loss <= loss + 1e-4 * t * (grad @ step) or t < 1e-8:
                break
            t *= 0.5
        w, loss, grad, hess = cand, new_loss, new_grad, new_hess
    return Linear(w * mask, mean, scale)


def heuristic() -> Linear:
    """The engine's built-in hand-set model, expressed over the full feature vector."""
    model = json.loads(etio.heuristic_model())
    idx = {name: i for i, name in enumerate(FEATURES)}
    w = np.zeros(len(FEATURES))
    mean = np.zeros(len(FEATURES))
    scale = np.ones(len(FEATURES))
    for name, m, s, wt in zip(model["features"], model["mean"], model["scale"], model["weights"], strict=True):
        w[idx[name]], mean[idx[name]], scale[idx[name]] = wt, m, s
    return Linear(w, mean, scale)


# -- cross-validation ---------------------------------------------------------------------


@dataclass
class Fold:
    held_out: str
    l2: float
    ranks: dict[str, list[int | None]] = field(default_factory=dict)
    datasets: list[str] = field(default_factory=list)


@dataclass
class CrossValidation:
    folds: list[Fold]
    best_l2: float
    by: str
    prior: bool = False

    def pooled(self, method: str) -> list[int | None]:
        return [r for f in self.folds for r in f.ranks[method]]

    def by_dataset(self) -> str:
        """Out-of-fold Avg@5 per dataset and suite, with 95% bootstrap intervals."""
        from etio.metrics import bootstrap  # noqa: PLC0415

        methods = list(self.folds[0].ranks) if self.folds else []
        rows: dict[str, dict[str, list[int | None]]] = {}
        for f in self.folds:
            for i, d in enumerate(f.datasets):
                for key in (d, d.split("-")[0]):
                    per = rows.setdefault(key, {m: [] for m in methods})
                    for m in methods:
                        per[m].append(f.ranks[m][i])
        lines = [
            "| dataset | n | " + " | ".join(f"Avg@5 {m}" for m in methods) + " |",
            "|---|---:|" + "---:|" * len(methods),
        ]
        for key in sorted(rows, key=lambda d: (d.split("-")[0], len(d), d)):
            per = rows[key]
            n = len(per[methods[0]])
            cells = " | ".join(bootstrap(per[m], average, n_boot=1000).fmt(3) for m in methods)
            lines.append(f"| {key} | {n} | {cells} |")
        return "\n".join(lines)

    def table(self) -> str:
        methods = list(self.folds[0].ranks) if self.folds else []
        head = f"| held-out {self.by} | n | chosen L2 | " + " | ".join(f"Avg@5 {m}" for m in methods) + " |"
        lines = [head, "|---|---:|---:|" + "---:|" * len(methods)]
        for f in self.folds:
            n = len(next(iter(f.ranks.values())))
            cells = " | ".join(f"{average(f.ranks[m]):.3f}" for m in methods)
            lines.append(f"| {f.held_out} | {n} | {f.l2:g} | {cells} |")
        n = len(self.pooled(methods[0])) if methods else 0
        cells = " | ".join(f"{average(self.pooled(m)):.3f}" for m in methods)
        lines.append(f"| **pooled** | {n} | {self.best_l2:g} | {cells} |")
        return "\n".join(lines)


def _key(g: Group, by: str) -> str:
    return g.system if by == "system" else g.suite


def _choose_l2(groups: Sequence[Group], by: str, augment: Sequence[Group] = (), prior: Linear | None = None) -> float:
    keys = sorted({_key(g, by) for g in groups})
    if len(keys) < 2:
        return 0.1
    best, best_score = L2_GRID[0], -1.0
    for l2 in L2_GRID:
        ranks: list[int | None] = []
        for k in keys:
            train = [g for g in [*groups, *augment] if _key(g, by) != k]
            test = [g for g in groups if _key(g, by) == k]
            model = fit(train, l2, prior=prior)
            ranks += [model.rank(g) for g in test]
        score = average(ranks)
        if score > best_score + 1e-12:
            best, best_score = l2, score
    return best


def cross_validate(
    groups: Sequence[Group], by: str = "system", augment: Sequence[Group] = (), prior: Linear | None = None
) -> CrossValidation:
    """Nested leave-one-group-out evaluation of the learned model.

    ``augment`` holds extra *training-only* groups: the same incidents with a
    telemetry source removed (modality dropout), so that the model does not
    come to depend on a source that some systems lack. Test folds always use
    the telemetry the held-out system actually has.

    Reports, on every held-out group, the learned model, the engine's
    heuristic model and a max-score ranking computed from the same features.
    """
    keys = sorted({_key(g, by) for g in groups})
    heur = heuristic()
    rel_max = FEATURES.index("rel_max")
    folds = []
    for k in keys:
        train = [g for g in groups if _key(g, by) != k]
        extra = [g for g in augment if _key(g, by) != k]
        test = [g for g in groups if _key(g, by) == k]
        l2 = _choose_l2(train, by, extra, prior)
        model = fit([*train, *extra], l2, prior=prior)
        folds.append(
            Fold(
                held_out=k,
                l2=l2,
                ranks={
                    "learned": [model.rank(g) for g in test],
                    "heuristic": [heur.rank(g) for g in test],
                    "ensemble": [rank_by_scores(g, ensemble_scores(model, heur, g)) for g in test],
                    "max_score": [rank_by_scores(g, g.x[:, rel_max]) for g in test],
                },
                datasets=[g.dataset for g in test],
            )
        )
    return CrossValidation(folds=folds, best_l2=_choose_l2(groups, by, augment, prior), by=by, prior=prior is not None)


def to_json(model: Linear, name: str, metadata: dict[str, str]) -> str:
    """Serialises a model in the engine's format and validates it natively."""
    keep = [i for i, f in enumerate(FEATURES) if f not in EXCLUDED_FEATURES]
    doc = {
        "format": "etio.rank-model/v1",
        "name": name,
        "feature_set": json.loads(etio.heuristic_model())["feature_set"],
        "features": [FEATURES[i] for i in keep],
        "mean": [float(model.mean[i]) for i in keep],
        "scale": [float(model.scale[i]) for i in keep],
        "weights": [float(model.weights[i]) for i in keep],
        "metadata": metadata,
    }
    return etio.validate_model(json.dumps(doc))


def fit_final(
    groups: Sequence[Group],
    *,
    l2: float,
    name: str,
    cv: CrossValidation,
    augment: Sequence[Group] = (),
    prior: Linear | None = None,
    sources: Sequence[str] = ("metrics",),
) -> str:
    """Fits on all groups (plus augmentation) and returns the model JSON with its provenance."""
    model = fit([*groups, *augment], l2, prior=prior)
    datasets = sorted({g.dataset for g in groups})
    meta = {
        "trained_on": ",".join(datasets),
        "sources": ",".join(sources),
        "training_cases": str(sum(1 for g in groups if g.y >= 0)),
        "dataset_revision": REVISION,
        "l2": f"{l2:g}",
        "cv": f"nested leave-one-{cv.by}-out",
        "cv_avg5_learned": f"{average(cv.pooled('learned')):.4f}",
        "cv_avg5_heuristic": f"{average(cv.pooled('heuristic')):.4f}",
        "augmentation": f"modality dropout ({len(augment)} groups without traces)" if augment else "none",
        "prior": "heuristic model (MAP, Gaussian)" if prior is not None else "none (ridge)",
        "etio_version": etio.__version__,
    }
    return to_json(model, name, meta)
