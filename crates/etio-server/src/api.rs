//! The HTTP API, server-sent events, Prometheus endpoint and web UI.
//!
//! | method & path | purpose |
//! |---|---|
//! | `GET /healthz` | liveness |
//! | `GET /readyz` | readiness (the engine answers) |
//! | `GET /metrics` | Prometheus metrics of the server |
//! | `GET /api/v1/status` | version, clock, counters |
//! | `GET /api/v1/services` | services with their health |
//! | `GET /api/v1/graph` | dependency graph |
//! | `GET /api/v1/series?service=` | series catalogue |
//! | `GET /api/v1/series/values?service=&name=&from=&to=` | series values |
//! | `GET /api/v1/incidents?limit=` | incidents, most recent first |
//! | `GET /api/v1/incidents/{id}` | one incident with its analysis |
//! | `POST /api/v1/incidents/{id}/feedback` | record the confirmed root cause |
//! | `POST /api/v1/analyze` | on-demand root-cause analysis |
//! | `POST /api/v1/alerts/alertmanager` | Prometheus Alertmanager webhook receiver |
//! | `GET /api/v1/events` | server-sent events for incidents |

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use etio_analysis::rca::{Method, RcaConfig};
use etio_core::{Resolution, Timestamp};
use etio_engine::{Engine, Event, Incident, IncidentStatus};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::actor::EngineHandle;
use crate::auth::Auth;
use crate::persist::Store;

/// Shared API state.
#[derive(Clone)]
pub struct ApiState {
    /// Engine handle.
    pub engine: EngineHandle,
    /// Durable store (feedback, incident history), if configured.
    pub store: Option<Arc<Store>>,
    /// Authentication.
    pub auth: Arc<Auth>,
    /// Role in a distributed deployment.
    pub role: crate::config::Role,
}

/// An API error rendered as `{"error": "..."}`.
#[derive(Debug)]
pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

fn unavailable<E: std::fmt::Display>(e: E) -> ApiError {
    ApiError(StatusCode::SERVICE_UNAVAILABLE, e.to_string())
}

#[derive(Serialize)]
struct Status {
    version: &'static str,
    role: crate::config::Role,
    now: i64,
    resolution_s: f64,
    lateness_s: f64,
    series: usize,
    stats: etio_engine::EngineStats,
}

async fn status(State(st): State<ApiState>) -> ApiResult<Status> {
    let role = st.role;
    st.engine
        .with_engine(move |e: &mut Engine| Status {
            version: env!("CARGO_PKG_VERSION"),
            role,
            now: e.now(),
            resolution_s: e.config().resolution.as_secs_f64(),
            lateness_s: e.config().lateness.as_secs_f64(),
            series: e.store().len(),
            stats: e.stats(),
        })
        .await
        .map(Json)
        .map_err(unavailable)
}

#[derive(Serialize)]
struct ServiceView {
    service: String,
    series: usize,
    anomalous: Vec<String>,
}

async fn services(State(st): State<ApiState>) -> ApiResult<Vec<ServiceView>> {
    st.engine
        .with_engine(|e: &mut Engine| {
            let store = e.store();
            let mut by: std::collections::BTreeMap<String, ServiceView> = std::collections::BTreeMap::new();
            for (i, m) in store.all_meta().iter().enumerate() {
                let v = by.entry(m.service.clone()).or_insert_with(|| ServiceView {
                    service: m.service.clone(),
                    series: 0,
                    anomalous: Vec::new(),
                });
                v.series += 1;
                #[allow(clippy::cast_possible_truncation)]
                if store.is_anomalous(i as u32) {
                    v.anomalous.push(m.name.clone());
                }
            }
            by.into_values().collect()
        })
        .await
        .map(Json)
        .map_err(unavailable)
}

#[derive(Serialize)]
struct GraphView {
    nodes: Vec<String>,
    edges: Vec<(String, String, f64)>,
}

async fn graph(State(st): State<ApiState>) -> ApiResult<GraphView> {
    st.engine
        .with_engine(|e: &mut Engine| {
            let g = e.graph();
            GraphView {
                nodes: g.names().to_vec(),
                edges: g.edges().map(|(a, b, w)| (g.name(a).to_owned(), g.name(b).to_owned(), w)).collect(),
            }
        })
        .await
        .map(Json)
        .map_err(unavailable)
}

#[derive(Deserialize)]
struct SeriesQuery {
    service: Option<String>,
}

async fn series(
    State(st): State<ApiState>,
    Query(q): Query<SeriesQuery>,
) -> ApiResult<Vec<etio_engine::store::SeriesMeta>> {
    st.engine
        .with_engine(move |e: &mut Engine| {
            e.store()
                .all_meta()
                .iter()
                .filter(|m| q.service.as_ref().is_none_or(|s| *s == m.service))
                .cloned()
                .collect()
        })
        .await
        .map(Json)
        .map_err(unavailable)
}

#[derive(Deserialize)]
struct ValuesQuery {
    service: String,
    name: String,
    /// Seconds since the epoch (default: one hour before `to`).
    from: Option<f64>,
    /// Seconds since the epoch (default: the latest window).
    to: Option<f64>,
}

#[derive(Serialize)]
struct Values {
    times: Vec<f64>,
    values: Vec<Option<f64>>,
}

async fn values(State(st): State<ApiState>, Query(q): Query<ValuesQuery>) -> Result<Json<Values>, ApiError> {
    let out = st
        .engine
        .with_engine(move |e: &mut Engine| {
            let id = e.store().find(&q.service, &q.name)?;
            let res: Resolution = e.config().resolution();
            let last = e.store().last_window()?;
            let to = q.to.map_or(last, |t| res.window_of(Timestamp::from_secs_f64(t)).0.min(last));
            let span = i64::try_from(e.store().capacity()).unwrap_or(i64::MAX) - 1;
            let from =
                q.from.map_or(to - span.min(360), |f| res.window_of(Timestamp::from_secs_f64(f)).0).max(to - span);
            let vals = e.store().read(id, from, to);
            Some(Values {
                times: (from..=to).map(|w| res.start_of(etio_core::time::WindowIdx(w)).as_secs_f64()).collect(),
                values: vals.into_iter().map(|v| v.is_finite().then_some(v)).collect(),
            })
        })
        .await
        .map_err(unavailable)?;
    out.map(Json).ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "no such series".into()))
}

#[derive(Serialize)]
struct IncidentSummary {
    id: String,
    status: IncidentStatus,
    start: i64,
    opened_at: i64,
    resolved_at: Option<i64>,
    services: usize,
    top: Option<(String, f64)>,
    analyses: u32,
}

impl From<&Incident> for IncidentSummary {
    fn from(i: &Incident) -> Self {
        Self {
            id: i.id.clone(),
            status: i.status,
            start: i.start,
            opened_at: i.opened_at,
            resolved_at: i.resolved_at,
            services: i.services.len(),
            top: i.top_candidate().map(|(s, p)| (s.to_owned(), p)),
            analyses: i.analyses,
        }
    }
}

#[derive(Deserialize)]
struct Limit {
    limit: Option<usize>,
}

async fn incidents(State(st): State<ApiState>, Query(q): Query<Limit>) -> ApiResult<Vec<IncidentSummary>> {
    let limit = q.limit.unwrap_or(50).min(500);
    st.engine
        .with_engine(move |e: &mut Engine| e.incidents().take(limit).map(IncidentSummary::from).collect())
        .await
        .map(Json)
        .map_err(unavailable)
}

async fn incident(State(st): State<ApiState>, Path(id): Path<String>) -> Result<Json<Incident>, ApiError> {
    let lookup = id.clone();
    let found = st.engine.with_engine(move |e: &mut Engine| e.incident(&lookup).cloned()).await.map_err(unavailable)?;
    if let Some(i) = found {
        return Ok(Json(i));
    }
    if let Some(store) = &st.store
        && let Some(i) = store.incident(&id).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        return Ok(Json(i));
    }
    Err(ApiError(StatusCode::NOT_FOUND, "no such incident".into()))
}

#[derive(Deserialize)]
struct Feedback {
    root_cause: String,
    #[serde(default)]
    comment: String,
}

async fn feedback(
    State(st): State<ApiState>,
    Path(id): Path<String>,
    Json(f): Json<Feedback>,
) -> Result<StatusCode, ApiError> {
    if f.root_cause.trim().is_empty() || f.root_cause.len() > 256 || f.comment.len() > 4096 {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "root_cause must be 1-256 characters, comment at most 4096".into(),
        ));
    }
    let lookup = id.clone();
    let exists =
        st.engine.with_engine(move |e: &mut Engine| e.incident(&lookup).is_some()).await.map_err(unavailable)?;
    let Some(store) = &st.store else {
        return Err(ApiError(StatusCode::NOT_IMPLEMENTED, "feedback needs storage.dir to be configured".into()));
    };
    if !exists && store.incident(&id).ok().flatten().is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such incident".into()));
    }
    store
        .record_feedback(&id, f.root_cause.trim(), &f.comment)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct AnalyzeRequest {
    /// Anomaly start, seconds since the epoch.
    anomaly_time: f64,
    /// Ranking method (default: the configured one).
    #[serde(default)]
    method: Option<Method>,
}

async fn analyze(
    State(st): State<ApiState>,
    Json(req): Json<AnalyzeRequest>,
) -> Result<Json<etio_analysis::RcaResult>, ApiError> {
    let result = st
        .engine
        .with_engine(move |e: &mut Engine| {
            let input = e.rca_input(Timestamp::from_secs_f64(req.anomaly_time).as_nanos())?;
            let cfg = RcaConfig { method: req.method.unwrap_or(e.config().rca.method), ..e.config().rca.clone() };
            etio_analysis::rca::analyze(&input, &cfg)
        })
        .await
        .map_err(unavailable)?;
    result.map(Json).map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))
}

/// The part of an Alertmanager webhook payload that matters here.
#[derive(Deserialize)]
struct AlertmanagerPayload {
    alerts: Vec<Alert>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Alert {
    status: String,
    starts_at: String,
    #[serde(default)]
    labels: std::collections::BTreeMap<String, String>,
}

#[derive(Serialize)]
struct AlertAnalysis {
    alert: std::collections::BTreeMap<String, String>,
    starts_at: String,
    ranking: Vec<(String, f64)>,
    error: Option<String>,
}

/// Parses the RFC 3339 timestamps Alertmanager sends (`2024-01-02T03:04:05.678Z`).
fn parse_rfc3339(s: &str) -> Option<f64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(str::parse::<i64>);
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let (clock, offset_s) = if let Some(c) = time.strip_suffix('Z') {
        (c, 0i64)
    } else {
        let pos = time.rfind(['+', '-'])?;
        let (c, off) = time.split_at(pos);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let (oh, om) = off[1..].split_once(':')?;
        (c, sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60))
    };
    let mut t = clock.split(':');
    let (hh, mm) = (t.next()?.parse::<i64>().ok()?, t.next()?.parse::<i64>().ok()?);
    let ss: f64 = t.next()?.parse().ok()?;
    // Days from civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    #[allow(clippy::cast_precision_loss)]
    Some((days * 86_400 + hh * 3600 + mm * 60 - offset_s) as f64 + ss)
}

async fn alertmanager(State(st): State<ApiState>, Json(p): Json<AlertmanagerPayload>) -> ApiResult<Vec<AlertAnalysis>> {
    let mut out = Vec::new();
    for alert in p.alerts.into_iter().filter(|a| a.status == "firing").take(20) {
        let Some(t) = parse_rfc3339(&alert.starts_at) else {
            out.push(AlertAnalysis {
                alert: alert.labels,
                starts_at: alert.starts_at,
                ranking: vec![],
                error: Some("unparseable startsAt".into()),
            });
            continue;
        };
        let r = st
            .engine
            .with_engine(move |e: &mut Engine| e.analyze_at(Timestamp::from_secs_f64(t).as_nanos()))
            .await
            .map_err(unavailable)?;
        let (ranking, error) = match r {
            Ok(res) => (res.ranking.iter().take(5).map(|x| (x.service.clone(), x.probability)).collect(), None),
            Err(e) => (Vec::new(), Some(e.to_string())),
        };
        out.push(AlertAnalysis { alert: alert.labels, starts_at: alert.starts_at, ranking, error });
    }
    Ok(Json(out))
}

async fn events(State(st): State<ApiState>) -> Sse<impl tokio_stream::Stream<Item = Result<SseEvent, Infallible>>> {
    let stream = BroadcastStream::new(st.engine.subscribe()).filter_map(|msg| {
        let event = msg.ok()?;
        let name = match &*event {
            Event::IncidentOpened { .. } => "incident_opened",
            Event::IncidentAnalyzed { .. } => "incident_analyzed",
            Event::IncidentResolved { .. } => "incident_resolved",
        };
        let data = serde_json::to_string(&*event).ok()?;
        Some(Ok(SseEvent::default().event(name).data(data)))
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

async fn metrics(State(st): State<ApiState>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/openmetrics-text; version=1.0.0; charset=utf-8")],
        st.engine.metrics().render(),
    )
        .into_response()
}

async fn ready(State(st): State<ApiState>) -> StatusCode {
    match tokio::time::timeout(Duration::from_secs(2), st.engine.with_engine(|_| ())).await {
        Ok(Ok(())) => StatusCode::OK,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// Builds the API router, optionally serving the web UI from `ui_dir`.
pub fn router(state: ApiState, ui_dir: Option<PathBuf>) -> Router {
    let api = Router::new()
        .route("/status", get(status))
        .route("/services", get(services))
        .route("/graph", get(graph))
        .route("/series", get(series))
        .route("/series/values", get(values))
        .route("/incidents", get(incidents))
        .route("/incidents/{id}", get(incident))
        .route("/incidents/{id}/feedback", post(feedback))
        .route("/analyze", post(analyze))
        .route("/alerts/alertmanager", post(alertmanager))
        .route("/events", get(events))
        .route_layer(axum::middleware::from_fn_with_state(state.auth.clone(), crate::auth::require_read));
    let mut app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(ready))
        .route("/metrics", get(metrics))
        .nest("/api/v1", api)
        .with_state(state);
    if let Some(dir) = ui_dir {
        let index = dir.join("index.html");
        app = app.fallback_service(
            tower_http::services::ServeDir::new(dir).fallback(tower_http::services::ServeFile::new(index)),
        );
    }
    app.layer(tower_http::trace::TraceLayer::new_for_http())
}

#[cfg(test)]
mod tests {
    use super::parse_rfc3339;

    #[test]
    fn parses_alertmanager_timestamps() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(parse_rfc3339("2024-01-02T03:04:05Z"), Some(1_704_164_645.0));
        let frac = parse_rfc3339("2024-01-02T03:04:05.5+01:00").unwrap();
        assert!((frac - (1_704_164_645.5 - 3600.0)).abs() < 1e-9);
        assert_eq!(parse_rfc3339("yesterday"), None);
    }
}
