"""Python bindings and evaluation harness for the Etio root-cause analysis engine.

The analysis itself is implemented in Rust (``etio._native``); this package adds
dataset adapters, evaluation metrics, model training and a command-line driver.
Everything that ranks root causes goes through the native code, so benchmark
numbers describe exactly what the engine does in production.
"""

from etio._native import (
    Detector,
    analyze,
    classify_metric,
    default_config,
    feature_names,
    heuristic_model,
    log_series,
    methods,
    service_features,
    trace_series,
    validate_model,
    version,
)

__version__ = version()

__all__ = [
    "Detector",
    "__version__",
    "analyze",
    "classify_metric",
    "default_config",
    "feature_names",
    "heuristic_model",
    "log_series",
    "methods",
    "service_features",
    "trace_series",
    "validate_model",
    "version",
]
