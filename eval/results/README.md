# Evaluation results

The results behind the tables in [docs/evaluation.md](../../docs/evaluation.md),
kept small enough to review. `eval/out/` (ignored by git) holds the full
outputs of local runs.

| file | contents |
|---|---|
| `rcaeval-methods.csv` | every method on every RCAEval case: rank of the true root cause (service level; `rank_published` in RCAEval's metric-level convention where it differs), runtime |
| `rcaeval-methods.meta.json` | provenance of that run: methods, configuration (the heuristic model, no ensemble), sources, dataset revision, versions |
| `sim-bench.json` | the 72 simulator scenarios of `etio sim bench`: fault, detection delay, rank at the first and final analysis, false incidents |

Regenerate with the commands at the end of the evaluation document.
