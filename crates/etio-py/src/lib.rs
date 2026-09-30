//! Python bindings for the Etio analysis core.
//!
//! The evaluation harness drives the *same* Rust code that the engine runs in
//! production. Inputs cross the boundary as NumPy arrays (read without
//! copying) and results come back as JSON documents, which keeps the binding
//! surface small and the Python side free of mirrored data classes.
#![allow(unsafe_code, missing_docs)] // PyO3 macro expansions; documented in the .pyi stub.

use etio_analysis::detect::{DetectorConfig, SeriesDetector};
use etio_analysis::graph::ServiceGraph;
use etio_analysis::rca::{self, FEATURE_NAMES, Method, RankModel, RcaConfig, RcaInput, SeriesInput};
use etio_core::{Direction, SignalCategory};
use numpy::{IntoPyArray, PyArray2, PyReadonlyArray1, PyReadonlyArray2};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

fn value_error(e: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(e.to_string())
}

fn parse_direction(s: &str) -> PyResult<Direction> {
    match s {
        "up" => Ok(Direction::Up),
        "down" => Ok(Direction::Down),
        "both" => Ok(Direction::Both),
        other => Err(value_error(format!("unknown direction `{other}`"))),
    }
}

#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
fn build_input(
    times: &PyReadonlyArray1<'_, f64>,
    values: &PyReadonlyArray2<'_, f64>,
    services: Vec<String>,
    names: Vec<String>,
    anomaly_time: f64,
    categories: Option<Vec<String>>,
    directions: Option<Vec<Option<String>>>,
    edges: Option<Vec<(String, String, f64)>>,
    exclude: Option<Vec<String>>,
) -> PyResult<RcaInput> {
    let times = times.as_array();
    let values = values.as_array();
    let (n_series, n_times) = values.dim();
    if n_times != times.len() {
        return Err(value_error(format!("values has {n_times} columns but times has {} entries", times.len())));
    }
    if services.len() != n_series || names.len() != n_series {
        return Err(value_error("services and names need one entry per row of values"));
    }
    if categories.as_ref().is_some_and(|c| c.len() != n_series)
        || directions.as_ref().is_some_and(|d| d.len() != n_series)
    {
        return Err(value_error("categories and directions need one entry per row of values"));
    }
    let mut series = Vec::with_capacity(n_series);
    for (i, (service, name)) in services.into_iter().zip(names).enumerate() {
        let category = match &categories {
            Some(c) => c[i].parse::<SignalCategory>().map_err(value_error)?,
            None => SignalCategory::classify_metric_name(&name),
        };
        let direction = match directions.as_ref().and_then(|d| d[i].as_deref()) {
            Some(d) => Some(parse_direction(d)?),
            None => None,
        };
        series.push(SeriesInput { service, name, category, direction, values: values.row(i).to_vec() });
    }
    let graph = edges.map(|edges| {
        let mut g = ServiceGraph::new();
        for (a, b, w) in edges {
            g.add_edge(&a, &b, w);
        }
        g
    });
    Ok(RcaInput { times: times.to_vec(), anomaly_time, series, graph, exclude: exclude.unwrap_or_default() })
}

fn parse_config(config: Option<&str>) -> PyResult<RcaConfig> {
    // Deserialising a config validates its ranking model.
    match config {
        Some(json) => serde_json::from_str(json).map_err(value_error),
        None => Ok(RcaConfig::default()),
    }
}

/// Ranks root-cause candidates; returns the result as a JSON string.
#[pyfunction]
#[pyo3(signature = (times, values, services, names, anomaly_time, *, categories=None, directions=None, edges=None, exclude=None, config=None))]
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
fn analyze(
    py: Python<'_>,
    times: PyReadonlyArray1<'_, f64>,
    values: PyReadonlyArray2<'_, f64>,
    services: Vec<String>,
    names: Vec<String>,
    anomaly_time: f64,
    categories: Option<Vec<String>>,
    directions: Option<Vec<Option<String>>>,
    edges: Option<Vec<(String, String, f64)>>,
    exclude: Option<Vec<String>>,
    config: Option<&str>,
) -> PyResult<String> {
    let input = build_input(&times, &values, services, names, anomaly_time, categories, directions, edges, exclude)?;
    let cfg = parse_config(config)?;
    let result = py.detach(|| rca::analyze(&input, &cfg)).map_err(value_error)?;
    serde_json::to_string(&result).map_err(value_error)
}

/// Computes the model features of every candidate service.
///
/// Returns `(services, matrix)` with one row per service in `feature_names()` order.
#[pyfunction]
#[pyo3(signature = (times, values, services, names, anomaly_time, *, categories=None, directions=None, edges=None, exclude=None, config=None))]
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value, clippy::type_complexity)]
fn service_features<'py>(
    py: Python<'py>,
    times: PyReadonlyArray1<'py, f64>,
    values: PyReadonlyArray2<'py, f64>,
    services: Vec<String>,
    names: Vec<String>,
    anomaly_time: f64,
    categories: Option<Vec<String>>,
    directions: Option<Vec<Option<String>>>,
    edges: Option<Vec<(String, String, f64)>>,
    exclude: Option<Vec<String>>,
    config: Option<&str>,
) -> PyResult<(Vec<String>, Bound<'py, PyArray2<f64>>)> {
    let input = build_input(&times, &values, services, names, anomaly_time, categories, directions, edges, exclude)?;
    let cfg = parse_config(config)?;
    let feats = py.detach(|| rca::service_features(&input, &cfg)).map_err(value_error)?;
    let names: Vec<String> = feats.iter().map(|f| f.service.clone()).collect();
    let n = FEATURE_NAMES.len();
    let flat: Vec<f64> = feats.iter().flat_map(|f| f.values.iter().copied()).collect();
    let matrix = numpy::ndarray::Array2::from_shape_vec((feats.len(), n), flat).map_err(value_error)?;
    Ok((names, matrix.into_pyarray(py)))
}

/// Output of the telemetry converters: `(times, values, services, names, categories, edges, stats)`.
type TableOutput<'py> = (
    Bound<'py, numpy::PyArray1<f64>>,
    Bound<'py, PyArray2<f64>>,
    Vec<String>,
    Vec<String>,
    Vec<&'static str>,
    Vec<(String, String, f64)>,
    String,
);

fn table_output<'py>(
    py: Python<'py>,
    table: etio_pipeline::batch::SeriesTable,
    stats: &etio_pipeline::batch::BatchStats,
) -> PyResult<TableOutput<'py>> {
    let times = table.times_secs();
    let n = table.series.len();
    let mut flat = Vec::with_capacity(n * table.len);
    let mut services = Vec::with_capacity(n);
    let mut names = Vec::with_capacity(n);
    let mut categories = Vec::with_capacity(n);
    for s in table.series {
        flat.extend_from_slice(&s.values);
        services.push(s.service);
        names.push(s.name);
        categories.push(s.category.as_str());
    }
    let matrix = numpy::ndarray::Array2::from_shape_vec((n, table.len), flat).map_err(value_error)?;
    #[allow(clippy::cast_precision_loss)]
    let edges = table.edges.into_iter().map(|(a, b, c)| (a, b, c as f64)).collect();
    Ok((
        numpy::PyArray1::from_vec(py, times),
        matrix.into_pyarray(py),
        services,
        names,
        categories,
        edges,
        serde_json::to_string(stats).map_err(value_error)?,
    ))
}

/// Parses a hexadecimal identifier; identifiers in other formats are hashed.
fn parse_id(s: &str) -> u128 {
    u128::from_str_radix(s.trim(), 16).unwrap_or_else(|_| {
        use std::hash::BuildHasher;
        let h = foldhash::fast::FixedState::with_seed(0x6574_696f).hash_one(s);
        u128::from(h) | (1 << 127)
    })
}

#[allow(clippy::cast_possible_truncation)]
fn secs_to_ts(s: f64) -> etio_core::Timestamp {
    etio_core::Timestamp::from_secs_f64(s)
}

fn resolution(seconds: f64) -> PyResult<etio_core::Resolution> {
    etio_core::Resolution::from_duration(std::time::Duration::from_secs_f64(seconds.max(1e-9))).map_err(value_error)
}

/// Converts recorded spans into per-service trace series (see `etio_pipeline::batch`).
///
/// Identifiers are hexadecimal strings (other formats are hashed). `parent_ids`
/// uses `None` or an empty string for root spans. Times are microseconds.
#[pyfunction]
#[pyo3(signature = (trace_ids, span_ids, parent_ids, services, operations, start_us, duration_us, errors, *, start_s, end_s, resolution_s=1.0))]
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
fn trace_series<'py>(
    py: Python<'py>,
    trace_ids: Vec<String>,
    span_ids: Vec<String>,
    parent_ids: Vec<Option<String>>,
    services: Vec<String>,
    operations: Vec<String>,
    start_us: PyReadonlyArray1<'py, i64>,
    duration_us: PyReadonlyArray1<'py, i64>,
    errors: PyReadonlyArray1<'py, bool>,
    start_s: f64,
    end_s: f64,
    resolution_s: f64,
) -> PyResult<TableOutput<'py>> {
    let n = trace_ids.len();
    let (start_us, duration_us, errors) = (start_us.as_array(), duration_us.as_array(), errors.as_array());
    if [
        span_ids.len(),
        parent_ids.len(),
        services.len(),
        operations.len(),
        start_us.len(),
        duration_us.len(),
        errors.len(),
    ]
    .iter()
    .any(|&l| l != n)
    {
        return Err(value_error("all span columns must have the same length"));
    }
    let interner = etio_core::Interner::default();
    #[allow(clippy::cast_possible_truncation)]
    let mut spans: Vec<etio_pipeline::Span> = (0..n)
        .map(|i| {
            let start = start_us[i].saturating_mul(1_000);
            etio_pipeline::Span {
                trace_id: parse_id(&trace_ids[i]),
                span_id: parse_id(&span_ids[i]) as u64,
                parent_id: parent_ids[i].as_deref().filter(|p| !p.is_empty()).map_or(0, |p| parse_id(p) as u64),
                service: interner.intern(&services[i]),
                operation: interner.intern(&operations[i]),
                kind: etio_pipeline::SpanKind::Unspecified,
                start,
                end: start.saturating_add(duration_us[i].max(0).saturating_mul(1_000)),
                status: if errors[i] { etio_pipeline::SpanStatus::Error } else { etio_pipeline::SpanStatus::Unset },
                peer: etio_core::Sym::EMPTY,
            }
        })
        .collect();
    let res = resolution(resolution_s)?;
    let (table, stats) = py.detach(|| {
        etio_pipeline::batch::trace_series(&mut spans, &interner, secs_to_ts(start_s), secs_to_ts(end_s), res)
    });
    table_output(py, table, &stats)
}

/// Converts recorded log lines into per-service log series (see `etio_pipeline::batch`).
#[pyfunction]
#[pyo3(signature = (timestamps_s, services, messages, *, start_s, end_s, novelty_after_s, resolution_s=1.0))]
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
fn log_series<'py>(
    py: Python<'py>,
    timestamps_s: PyReadonlyArray1<'py, f64>,
    services: Vec<String>,
    messages: Vec<String>,
    start_s: f64,
    end_s: f64,
    novelty_after_s: f64,
    resolution_s: f64,
) -> PyResult<TableOutput<'py>> {
    let ts = timestamps_s.as_array();
    if services.len() != ts.len() || messages.len() != ts.len() {
        return Err(value_error("all log columns must have the same length"));
    }
    let mut records: Vec<etio_pipeline::batch::LogRecord<'_>> = (0..ts.len())
        .map(|i| etio_pipeline::batch::LogRecord {
            ts: secs_to_ts(ts[i]).as_nanos(),
            service: &services[i],
            message: &messages[i],
            severity: None,
        })
        .collect();
    let res = resolution(resolution_s)?;
    let (table, stats) = py.detach(|| {
        etio_pipeline::batch::log_series(
            &mut records,
            secs_to_ts(start_s),
            secs_to_ts(end_s),
            res,
            secs_to_ts(novelty_after_s),
            etio_pipeline::logs::DrainConfig::default(),
        )
    });
    table_output(py, table, &stats)
}

/// Feature names, in the column order of `service_features`.
#[pyfunction]
fn feature_names() -> Vec<&'static str> {
    FEATURE_NAMES.to_vec()
}

/// Names of the available ranking methods.
#[pyfunction]
fn methods() -> Vec<&'static str> {
    Method::ALL.iter().map(|m| m.as_str()).collect()
}

/// The default analysis configuration as JSON.
#[pyfunction]
fn default_config() -> PyResult<String> {
    serde_json::to_string_pretty(&RcaConfig::default()).map_err(value_error)
}

/// The built-in hand-set ranking model as JSON.
#[pyfunction]
fn heuristic_model() -> String {
    RankModel::heuristic().to_json()
}

/// Validates a ranking model JSON and returns it normalised.
#[pyfunction]
fn validate_model(json: &str) -> PyResult<String> {
    let m = RankModel::from_json(json).map_err(value_error)?;
    Ok(m.to_json())
}

/// The metric classification rules, exposed for dataset adapters.
#[pyfunction]
fn classify_metric(name: &str) -> &'static str {
    SignalCategory::classify_metric_name(name).as_str()
}

/// Version of the native library.
#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Streaming anomaly detector for one series (the engine's per-series detector).
#[pyclass(module = "etio._native")]
struct Detector {
    inner: SeriesDetector,
}

#[pymethods]
impl Detector {
    #[new]
    #[pyo3(signature = (direction="up", config=None))]
    fn new(direction: &str, config: Option<&str>) -> PyResult<Self> {
        let cfg: DetectorConfig = match config {
            Some(json) => serde_json::from_str(json).map_err(value_error)?,
            None => DetectorConfig::default(),
        };
        Ok(Self { inner: SeriesDetector::new(cfg, parse_direction(direction)?) })
    }

    /// Feeds one value; returns `(state, z, p, surprise, episode_age)`.
    fn observe(&mut self, x: f64) -> (String, f64, f64, f64, u32) {
        let o = self.inner.observe(x);
        let state = serde_json::to_value(o.state).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
        (state, o.z, o.p, o.surprise, o.episode_age)
    }

    /// Feeds a whole array; returns the surprise and an anomaly flag per value.
    #[allow(clippy::needless_pass_by_value)]
    fn observe_many(&mut self, xs: PyReadonlyArray1<'_, f64>) -> (Vec<f64>, Vec<bool>) {
        let xs = xs.as_array();
        let mut surprise = Vec::with_capacity(xs.len());
        let mut anomalous = Vec::with_capacity(xs.len());
        for &x in &xs {
            let o = self.inner.observe(x);
            surprise.push(o.surprise);
            anomalous.push(o.state == etio_analysis::SeriesState::Anomalous);
        }
        (surprise, anomalous)
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(analyze, m)?)?;
    m.add_function(wrap_pyfunction!(service_features, m)?)?;
    m.add_function(wrap_pyfunction!(trace_series, m)?)?;
    m.add_function(wrap_pyfunction!(log_series, m)?)?;
    m.add_function(wrap_pyfunction!(feature_names, m)?)?;
    m.add_function(wrap_pyfunction!(methods, m)?)?;
    m.add_function(wrap_pyfunction!(default_config, m)?)?;
    m.add_function(wrap_pyfunction!(validate_model, m)?)?;
    m.add_function(wrap_pyfunction!(heuristic_model, m)?)?;
    m.add_function(wrap_pyfunction!(classify_metric, m)?)?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_class::<Detector>()?;
    Ok(())
}
