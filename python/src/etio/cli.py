"""``etio-eval``: download benchmarks, run methods, train the ranking model."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import etio
from etio import benchmark, report
from etio.datasets.rcaeval import RcaEval


def _progress(i: int, n: int, name: str) -> None:
    if i == n or i % 10 == 0:
        print(f"  [{i}/{n}] {name}", file=sys.stderr, flush=True)


def cmd_fetch(args: argparse.Namespace) -> int:
    ds = RcaEval(args.data)
    cases = ds.cases(args.datasets)
    for i, c in enumerate(cases):
        paths = [f"{c.name}/metrics.parquet"]
        if args.logs and c.has_logs:
            paths.append(f"{c.name}/logs.parquet")
        if args.traces and c.has_traces:
            paths.append(f"{c.name}/traces.parquet")
        ds.prefetch(paths)
        _progress(i + 1, len(cases), c.name)
    print(f"{len(cases)} cases available under {ds.dir}")
    return 0


def _load_json(path: str | None) -> dict | None:
    return json.loads(Path(path).read_text()) if path else None


def cmd_run(args: argparse.Namespace) -> int:
    ds = RcaEval(args.data)
    cases = ds.cases(args.datasets)
    if args.limit:
        cases = cases[: args.limit]
    config = _load_json(args.config) or {}
    model_source = "config" if "model" in config else "heuristic"
    if args.model:
        config["model"] = json.loads(Path(args.model).read_text())
        model_source = args.model
    elif args.bundled_model:
        model_source = "bundled"
        print(
            "warning: the bundled model was trained on RCAEval; scores on RCAEval are in-sample "
            "(use `etio-eval train` for cross-validated estimates)",
            file=sys.stderr,
        )
    elif "model" not in config:
        # Guard against evaluation leakage: the bundled model and the default
        # ensemble were fitted on these very cases.
        config["model"] = json.loads(etio.heuristic_model())
        config["ensemble"] = []
    graphs = _load_json(args.graphs)
    print(f"running {len(args.methods)} methods on {len(cases)} cases", file=sys.stderr)
    results = benchmark.run(
        ds,
        cases,
        args.methods,
        config=config,
        graphs=graphs,
        workers=args.workers,
        sources=args.sources,
        progress=_progress,
    )
    out = Path(args.out)
    meta = {
        "methods": args.methods,
        "config": config,
        "model": model_source,
        "graphs": bool(graphs),
        "sources": args.sources,
    }
    benchmark.save(results, out, meta=meta)
    print(report.table(results, methods=args.methods))
    print(f"\nresults written to {out}", file=sys.stderr)
    return 0


def cmd_report(args: argparse.Namespace) -> int:
    _, results = benchmark.load(Path(args.results))
    print(report.table(results, published=args.published))
    if args.compare:
        a, b = args.compare
        print()
        print(report.comparison(results, a, b))
    return 0


def cmd_train(args: argparse.Namespace) -> int:
    from etio import ltr  # noqa: PLC0415 - heavier import

    ds = RcaEval(args.data)
    cases = ds.cases(args.datasets)
    graphs = _load_json(args.graphs)
    config = _load_json(args.config) or {}
    data = ltr.build_dataset(
        ds,
        cases,
        config=config,
        graphs=graphs,
        sources=args.sources,
        workers=args.workers,
        progress=_progress,
        cache_dir=args.cache,
    )
    augment = []
    if args.augment and "traces" in args.sources:
        # Modality dropout: incidents that have traces also train the model
        # without them, so it keeps working on systems without tracing.
        traced = [c for c in cases if c.has_traces]
        reduced = [s for s in args.sources if s != "traces"]
        print(f"augmenting with {len(traced)} incidents without traces", file=sys.stderr)
        augment = ltr.build_dataset(
            ds,
            traced,
            config=config,
            graphs=graphs,
            sources=reduced,
            workers=args.workers,
            progress=_progress,
            cache_dir=args.cache,
        )
    prior = ltr.heuristic() if args.prior == "heuristic" else None
    cv = ltr.cross_validate(data, by=args.cv, augment=augment, prior=prior)
    print(cv.table())
    print()
    print(cv.by_dataset())
    model = ltr.fit_final(
        data, l2=cv.best_l2, name=args.name, cv=cv, augment=augment, prior=prior, sources=args.sources
    )
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out).write_text(model)
    print(f"\nmodel written to {args.out}", file=sys.stderr)
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="etio-eval", description=__doc__)
    p.add_argument("--data", help="dataset directory (default: $ETIO_DATA_DIR/rcaeval or ./data/rcaeval)")
    sub = p.add_subparsers(dest="cmd", required=True)

    f = sub.add_parser("fetch", help="download and verify benchmark cases")
    f.add_argument("--datasets", nargs="*", default=None, help="e.g. RE1 RE2-OB (default: all)")
    f.add_argument("--logs", action="store_true")
    f.add_argument("--traces", action="store_true")
    f.set_defaults(fn=cmd_fetch)

    r = sub.add_parser("run", help="run methods and print a summary table")
    r.add_argument("--datasets", nargs="*", default=["RE1", "RE2", "RE3"])
    r.add_argument("--methods", nargs="+", default=["etio", "baro", "nsigma", "max_score"])
    r.add_argument("--config", help="JSON analysis config")
    r.add_argument("--model", help="JSON ranking model (default: the heuristic model, to avoid leakage)")
    r.add_argument(
        "--bundled-model",
        action="store_true",
        help="score with the bundled learned model and ensemble (in-sample on RCAEval)",
    )
    r.add_argument("--graphs", help="JSON {system: [[caller, callee, weight], ...]}")
    r.add_argument("--sources", nargs="+", default=["metrics"], choices=["metrics", "traces", "logs"])
    r.add_argument("--limit", type=int)
    r.add_argument("--workers", type=int, default=2)
    r.add_argument("--out", default="eval/out/run.json")
    r.set_defaults(fn=cmd_run)

    rep = sub.add_parser("report", help="summarise a results file")
    rep.add_argument("results")
    rep.add_argument("--published", action="store_true", help="use RCAEval's metric-level convention")
    rep.add_argument("--compare", nargs=2, metavar=("A", "B"))
    rep.set_defaults(fn=cmd_report)

    t = sub.add_parser("train", help="train the ranking model with cross-validation")
    t.add_argument("--datasets", nargs="*", default=["RE1", "RE2", "RE3"])
    t.add_argument("--graphs")
    t.add_argument("--config")
    t.add_argument("--sources", nargs="+", default=["metrics"], choices=["metrics", "traces", "logs"])
    t.add_argument("--cv", choices=["system", "suite"], default="system")
    t.add_argument("--name", default="etio-rank")
    t.add_argument("--workers", type=int, default=2)
    t.add_argument("--out", default="models/etio-rank.json")
    t.add_argument("--cache", default="eval/out/cache", help="feature cache directory ('' disables)")
    t.add_argument("--augment", action="store_true", help="modality-dropout augmentation (hurt in our evaluation)")
    t.add_argument("--prior", choices=["heuristic", "none"], default="heuristic", help="centre of the weight penalty")
    t.set_defaults(fn=cmd_train)

    args = p.parse_args(argv)
    return int(args.fn(args))


if __name__ == "__main__":
    raise SystemExit(main())
