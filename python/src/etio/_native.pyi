"""Type stubs for the native extension."""

import numpy as np
import numpy.typing as npt

def analyze(
    times: npt.NDArray[np.float64],
    values: npt.NDArray[np.float64],
    services: list[str],
    names: list[str],
    anomaly_time: float,
    *,
    categories: list[str] | None = None,
    directions: list[str | None] | None = None,
    edges: list[tuple[str, str, float]] | None = None,
    exclude: list[str] | None = None,
    config: str | None = None,
) -> str:
    """Rank root-cause candidates. ``values`` has one row per series.

    Returns the result as a JSON document (see ``RcaResult`` in the Rust docs).
    """

def service_features(
    times: npt.NDArray[np.float64],
    values: npt.NDArray[np.float64],
    services: list[str],
    names: list[str],
    anomaly_time: float,
    *,
    categories: list[str] | None = None,
    directions: list[str | None] | None = None,
    edges: list[tuple[str, str, float]] | None = None,
    exclude: list[str] | None = None,
    config: str | None = None,
) -> tuple[list[str], npt.NDArray[np.float64]]:
    """Model features of every candidate, one row per service."""

TableOutput = tuple[
    npt.NDArray[np.float64],  # window start times, seconds
    npt.NDArray[np.float64],  # values, one row per series
    list[str],  # services
    list[str],  # series names
    list[str],  # categories
    list[tuple[str, str, float]],  # dependency edges (caller, callee, calls)
    str,  # conversion statistics as JSON
]

def trace_series(
    trace_ids: list[str],
    span_ids: list[str],
    parent_ids: list[str | None],
    services: list[str],
    operations: list[str],
    start_us: npt.NDArray[np.int64],
    duration_us: npt.NDArray[np.int64],
    errors: npt.NDArray[np.bool_],
    *,
    start_s: float,
    end_s: float,
    resolution_s: float = 1.0,
) -> TableOutput:
    """Per-service trace series (requests, errors, latency, local time, error origins)."""

def log_series(
    timestamps_s: npt.NDArray[np.float64],
    services: list[str],
    messages: list[str],
    *,
    start_s: float,
    end_s: float,
    novelty_after_s: float,
    resolution_s: float = 1.0,
) -> TableOutput:
    """Per-service log series (lines, error lines, lines of novel templates)."""

def feature_names() -> list[str]: ...
def methods() -> list[str]: ...
def default_config() -> str: ...
def validate_model(json: str) -> str: ...
def heuristic_model() -> str: ...
def classify_metric(name: str) -> str: ...
def version() -> str: ...

class Detector:
    """The engine's streaming per-series anomaly detector."""

    def __init__(self, direction: str = "up", config: str | None = None) -> None: ...
    def observe(self, x: float) -> tuple[str, float, float, float, int]:
        """Returns ``(state, z, p, surprise, episode_age)``."""
    def observe_many(self, xs: npt.NDArray[np.float64]) -> tuple[list[float], list[bool]]:
        """Returns the surprise and an anomaly flag for every value."""
