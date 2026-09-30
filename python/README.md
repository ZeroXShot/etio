# etio (Python)

Python bindings and evaluation harness for the [Etio](../README.md) root-cause
analysis engine. The ranking itself runs in Rust; this package adds benchmark
adapters, metrics with bootstrap intervals, model training and the `etio-eval`
command.

```bash
uv sync --extra baselines          # builds the native extension with maturin
uv run etio-eval run --datasets RE1 --methods etio baro rcaeval_baro
uv run etio-eval train --datasets RE1 RE2 RE3 --out ../models/etio-rank.json
```

`etio-eval run` scores the `etio` method with the hand-set heuristic model
unless `--model` is given: the model bundled with Etio was trained on
RCAEval, so its scores there would be in-sample (`--bundled-model` uses it
anyway, with a warning). Cross-validated estimates of the learned model come
from `etio-eval train`.

The bindings can also be used directly:

```python
import json, numpy as np, etio

times = np.arange(0.0, 900.0, 10.0)                    # seconds
values = np.vstack([latency_frontend, latency_cart])   # one row per series
result = json.loads(etio.analyze(times, values, ["frontend", "cart"], ["latency_p95"] * 2, 600.0))
print(result["ranking"][0]["service"])
```

Set `ETIO_DATA_DIR` to choose where datasets are downloaded (default
`./data`). See [`docs/evaluation.md`](../docs/evaluation.md) for the
methodology.
