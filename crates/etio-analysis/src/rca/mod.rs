//! Root-cause ranking from a window of telemetry.
//!
//! The entry point is [`analyze`]: given aligned series around an anomaly
//! time, it returns candidate services ranked by how likely each is to be
//! the root cause, with the evidence behind every position.
//!
//! Several methods are available. [`Method::Etio`] is the production method
//! (features + ranking model); the others are faithful re-implementations of
//! published baselines, kept here so that the evaluation harness compares
//! methods on identical inputs and identical preprocessing.

pub mod explain;
pub mod features;
pub mod model;
pub mod score;

use std::collections::BTreeMap;

use etio_core::{Direction, SignalCategory};
use serde::{Deserialize, Serialize};

use crate::graph::{ServiceGraph, WalkConfig, anomaly_random_walk};
pub use features::{FEATURE_NAMES, FEATURE_SET_VERSION, ServiceFeatures};
pub use model::{MODEL_FORMAT, ModelError, RankModel};
use score::{Scaling, ScoreConfig, SeriesScore, Split};

/// One telemetry series in an analysis window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeriesInput {
    /// Service (or other root-cause candidate) that owns the series.
    pub service: String,
    /// Series name, for example `latency_p95` or `container_cpu`.
    pub name: String,
    /// Semantic class; decides the harmful direction and feature group.
    pub category: SignalCategory,
    /// Overrides the category's default harmful direction.
    #[serde(default)]
    pub direction: Option<Direction>,
    /// One value per entry of [`RcaInput::times`]; NaN marks missing data.
    pub values: Vec<f64>,
}

impl SeriesInput {
    /// Builds a series, classifying its category from its name.
    #[must_use]
    pub fn new(service: impl Into<String>, name: impl Into<String>, values: Vec<f64>) -> Self {
        let name = name.into();
        let category = SignalCategory::classify_metric_name(&name);
        Self { service: service.into(), name, category, direction: None, values }
    }

    fn direction(&self) -> Direction {
        self.direction.unwrap_or_else(|| self.category.default_direction())
    }
}

/// Everything [`analyze`] needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RcaInput {
    /// Sample times in seconds since the Unix epoch, strictly increasing.
    pub times: Vec<f64>,
    /// When the incident started (or was detected), in seconds since the epoch.
    pub anomaly_time: f64,
    /// The series, aligned on `times`.
    pub series: Vec<SeriesInput>,
    /// Caller → callee dependencies, if known.
    #[serde(default)]
    pub graph: Option<ServiceGraph>,
    /// Services that must never be blamed (load generators, sidecars).
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// Ranking method.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// Etio: multi-source features scored by the ranking model.
    #[default]
    Etio,
    /// Maximum robust score per service (Etio's scaling, no model): an ablation.
    MaxScore,
    /// BARO's robust scorer (Pham et al., FSE 2024): median/IQR scaling,
    /// services ranked by their most deviating metric.
    Baro,
    /// N-sigma: mean/standard-deviation scaling, services ranked by their most
    /// deviating metric.
    #[serde(rename = "nsigma")]
    NSigma,
    /// Random walk with restart on the anomaly-weighted dependency graph.
    RandomWalk,
}

impl Method {
    /// All methods.
    pub const ALL: [Self; 5] = [Self::Etio, Self::MaxScore, Self::Baro, Self::NSigma, Self::RandomWalk];

    /// Stable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Etio => "etio",
            Self::MaxScore => "max_score",
            Self::Baro => "baro",
            Self::NSigma => "nsigma",
            Self::RandomWalk => "random_walk",
        }
    }
}

impl std::str::FromStr for Method {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|m| m.as_str() == s).ok_or_else(|| format!("unknown method `{s}`"))
    }
}

/// Analysis settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RcaConfig {
    /// Ranking method.
    pub method: Method,
    /// Series scoring settings.
    pub score: ScoreConfig,
    /// Random-walk settings.
    pub walk: WalkConfig,
    /// Ranking model for [`Method::Etio`].
    pub model: RankModel,
    /// Additional models combined with `model` as a product of experts: each
    /// model's scores are standardised over the candidates of the incident
    /// and summed. Combining the trained model with the hand-set one trades a
    /// little average accuracy for much better worst-case accuracy on systems
    /// unlike those seen in training (see `docs/evaluation.md`).
    pub ensemble: Vec<RankModel>,
    /// Longest reference period used before the anomaly, in seconds.
    pub reference_window_s: Option<f64>,
    /// Longest abnormal period used after the anomaly, in seconds.
    pub abnormal_window_s: Option<f64>,
    /// Evidence series reported per service.
    pub signals_per_service: usize,
    /// Services reported (all are ranked; this limits the output).
    pub max_services: usize,
}

impl Default for RcaConfig {
    fn default() -> Self {
        Self {
            method: Method::Etio,
            score: ScoreConfig::default(),
            walk: WalkConfig::default(),
            model: RankModel::bundled(),
            ensemble: vec![RankModel::heuristic()],
            reference_window_s: None,
            abnormal_window_s: None,
            signals_per_service: 5,
            max_services: 20,
        }
    }
}

/// Invalid analysis input.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RcaError {
    /// No series were given.
    #[error("no series to analyse")]
    NoSeries,
    /// `times` is empty, not finite or not strictly increasing.
    #[error("times must be finite and strictly increasing")]
    BadTimes,
    /// A series does not have one value per time.
    #[error("series `{service}/{name}` has {got} values, expected {expected}")]
    Length {
        /// Service of the offending series.
        service: String,
        /// Name of the offending series.
        name: String,
        /// Values supplied.
        got: usize,
        /// Values expected.
        expected: usize,
    },
    /// The anomaly time leaves no reference or no abnormal data.
    #[error("anomaly time {0} leaves no reference or no abnormal period")]
    AnomalyTime(f64),
}

/// Evidence from one series.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    /// Owning service.
    pub service: String,
    /// Series name.
    pub name: String,
    /// Category.
    pub category: SignalCategory,
    /// Scores of the series.
    pub score: SeriesScore,
}

/// One ranked candidate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RankedService {
    /// 1-based position.
    pub rank: usize,
    /// Service name.
    pub service: String,
    /// Method-specific score (higher is more likely).
    pub score: f64,
    /// Probability under the model's softmax ([`Method::Etio`]) or the
    /// normalised score (other methods).
    pub probability: f64,
    /// Per-feature contributions to the score ([`Method::Etio`] only).
    pub contributions: Vec<(String, f64)>,
    /// Feature values.
    pub features: BTreeMap<String, f64>,
    /// The most anomalous series of the service.
    pub signals: Vec<Signal>,
    /// Plain-language reasons, strongest first.
    pub reasons: Vec<String>,
}

/// Result of an analysis.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RcaResult {
    /// Method used.
    pub method: Method,
    /// Ranking model name ([`Method::Etio`] only).
    pub model: Option<String>,
    /// Anomaly time, seconds since the epoch.
    pub anomaly_time: f64,
    /// Reference period used, seconds since the epoch.
    pub reference: (f64, f64),
    /// Abnormal period used, seconds since the epoch.
    pub abnormal: (f64, f64),
    /// Ranked candidates, most likely first.
    pub ranking: Vec<RankedService>,
    /// Series that could not be scored.
    pub skipped_series: usize,
    /// Non-fatal observations about the input.
    pub warnings: Vec<String>,
}

impl RcaResult {
    /// Candidate names in rank order.
    #[must_use]
    pub fn services(&self) -> Vec<&str> {
        self.ranking.iter().map(|r| r.service.as_str()).collect()
    }

    /// 1-based rank of a service, if ranked.
    #[must_use]
    pub fn rank_of(&self, service: &str) -> Option<usize> {
        self.ranking.iter().find(|r| r.service == service).map(|r| r.rank)
    }
}

fn split(input: &RcaInput, cfg: &RcaConfig) -> Result<(Split, Vec<f64>), RcaError> {
    let times = &input.times;
    if times.is_empty() || times.iter().any(|t| !t.is_finite()) || times.windows(2).any(|w| w[1] <= w[0]) {
        return Err(RcaError::BadTimes);
    }
    let t0 = input.anomaly_time;
    let abnormal_start = times.partition_point(|&t| t < t0);
    let reference_start = cfg.reference_window_s.map_or(0, |w| times.partition_point(|&t| t < t0 - w));
    let abnormal_end =
        cfg.abnormal_window_s.map_or(times.len(), |w| times.partition_point(|&t| t < t0 + w).max(abnormal_start + 1));
    if abnormal_start == 0 || abnormal_start >= times.len() || reference_start >= abnormal_start {
        return Err(RcaError::AnomalyTime(t0));
    }
    let rel = times.iter().map(|t| t - t0).collect();
    Ok((Split { reference_start, abnormal_start, abnormal_end: abnormal_end.min(times.len()) }, rel))
}

/// Scored series and service features for one input, shared by [`analyze`]
/// and [`service_features`] so that training and inference cannot diverge.
struct Prepared<'a> {
    split: Split,
    scored: Vec<features::ScoredSeries<'a>>,
    feats: Vec<ServiceFeatures>,
    skipped: usize,
    warnings: Vec<String>,
}

fn prepare<'a>(input: &'a RcaInput, cfg: &RcaConfig) -> Result<Prepared<'a>, RcaError> {
    if input.series.is_empty() {
        return Err(RcaError::NoSeries);
    }
    let n = input.times.len();
    for s in &input.series {
        if s.values.len() != n {
            return Err(RcaError::Length {
                service: s.service.clone(),
                name: s.name.clone(),
                got: s.values.len(),
                expected: n,
            });
        }
    }
    let (sp, rel_times) = split(input, cfg)?;
    let scaling = match cfg.method {
        Method::Baro => Scaling::Iqr,
        Method::NSigma => Scaling::NSigma,
        Method::Etio | Method::MaxScore | Method::RandomWalk => Scaling::Robust,
    };

    let excluded = |svc: &str| input.exclude.iter().any(|e| e == svc);
    let mut scored = Vec::with_capacity(input.series.len());
    let mut skipped = 0usize;
    let mut silent: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for s in &input.series {
        if excluded(&s.service) {
            continue;
        }
        let entry = silent.entry(s.service.as_str()).or_insert((0, 0));
        entry.1 += 1;
        if score::went_silent(&s.values, sp) {
            entry.0 += 1;
        }
        match score::score_series(&s.values, &rel_times, sp, s.direction(), scaling, &cfg.score) {
            Some(score) => {
                scored.push(features::ScoredSeries { service: &s.service, name: &s.name, category: s.category, score });
            }
            None => skipped += 1,
        }
    }
    let mut warnings = Vec::new();
    if scored.is_empty() {
        warnings.push("no series could be scored; the ranking is uninformative".to_owned());
    }
    if skipped > 0 {
        warnings.push(format!("{skipped} series lacked reference data and were skipped"));
    }
    let graph = input.graph.as_ref().filter(|g| !g.is_empty());
    let feats: Vec<ServiceFeatures> = features::compute(&scored, &silent, graph, cfg.score.threshold, &cfg.walk)
        .into_iter()
        .filter(|f| !excluded(&f.service))
        .collect();
    Ok(Prepared { split: sp, scored, feats, skipped, warnings })
}

/// Ranks root-cause candidates.
///
/// # Errors
/// Returns an [`RcaError`] if the input is malformed. Series that cannot be
/// scored (too little reference data) are skipped and counted, not errors.
#[allow(clippy::cast_precision_loss)]
pub fn analyze(input: &RcaInput, cfg: &RcaConfig) -> Result<RcaResult, RcaError> {
    let Prepared { split: sp, scored, feats, skipped, mut warnings } = prepare(input, cfg)?;
    let graph = input.graph.as_ref().filter(|g| !g.is_empty());

    // Method-specific scores.
    let (scores, contributions): (Vec<f64>, Vec<Vec<(String, f64)>>) = match cfg.method {
        Method::Etio => ensemble_scores(&cfg.model, &cfg.ensemble, &feats),
        Method::MaxScore | Method::Baro | Method::NSigma => {
            (feats.iter().map(|f| f.max_score).collect(), vec![Vec::new(); feats.len()])
        }
        Method::RandomWalk => {
            let walk = graph.map(|g| {
                let global = feats.iter().map(|f| f.max_score).fold(0.0, f64::max);
                let node_scores: Vec<f64> = g
                    .names()
                    .iter()
                    .map(|name| {
                        feats
                            .iter()
                            .find(|f| &f.service == name)
                            .map_or(0.0, |f| if global > 0.0 { f.max_score / global } else { 0.0 })
                    })
                    .collect();
                anomaly_random_walk(g, &node_scores, &cfg.walk)
            });
            if walk.is_none() {
                warnings.push("random walk needs a dependency graph; fell back to max score".to_owned());
            }
            let scores = feats
                .iter()
                .map(|f| match (&walk, graph.and_then(|g| g.node(&f.service))) {
                    // Break ties between walk scores with the anomaly score.
                    (Some(pi), Some(node)) => pi[node] + 1e-9 * f.max_score,
                    (Some(_), None) => 1e-9 * f.max_score,
                    (None, _) => f.max_score,
                })
                .collect();
            (scores, vec![Vec::new(); feats.len()])
        }
    };

    let probabilities = if cfg.method == Method::Etio {
        model::softmax(&scores)
    } else {
        let total: f64 = scores.iter().map(|s| s.max(0.0)).sum();
        scores.iter().map(|s| if total > 0.0 { s.max(0.0) / total } else { 0.0 }).collect()
    };

    // Deterministic order: score, then name.
    let mut order: Vec<usize> = (0..feats.len()).collect();
    order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]).then_with(|| feats[a].service.cmp(&feats[b].service)));

    let mut signals_by_service: BTreeMap<&str, Vec<&features::ScoredSeries<'_>>> = BTreeMap::new();
    for s in &scored {
        signals_by_service.entry(s.service).or_default().push(s);
    }
    for v in signals_by_service.values_mut() {
        v.sort_by(|a, b| b.score.max.total_cmp(&a.score.max).then_with(|| a.name.cmp(b.name)));
    }

    let ranking: Vec<RankedService> = order
        .iter()
        .take(cfg.max_services.max(1))
        .enumerate()
        .map(|(pos, &i)| {
            let f = &feats[i];
            let signals: Vec<Signal> = signals_by_service
                .get(f.service.as_str())
                .map(|v| {
                    v.iter()
                        .take(cfg.signals_per_service)
                        .map(|s| Signal {
                            service: s.service.to_owned(),
                            name: s.name.to_owned(),
                            category: s.category,
                            score: s.score.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let features = f.as_map();
            let reasons = explain::reasons(&features, &contributions[i], &signals, cfg.score.threshold);
            RankedService {
                rank: pos + 1,
                service: f.service.clone(),
                score: scores[i],
                probability: probabilities[i],
                contributions: contributions[i].clone(),
                features,
                signals,
                reasons,
            }
        })
        .collect();

    let t = &input.times;
    Ok(RcaResult {
        method: cfg.method,
        model: (cfg.method == Method::Etio).then(|| cfg.model.name.clone()),
        anomaly_time: input.anomaly_time,
        reference: (t[sp.reference_start], t[sp.abnormal_start - 1]),
        abnormal: (t[sp.abnormal_start], t[sp.abnormal_end - 1]),
        ranking,
        skipped_series: skipped,
        warnings,
    })
}

/// Scores candidates with one model, or with a product of experts when
/// `extra` models are given: every model's logits are standardised over the
/// candidates (zero mean, unit variance) and summed. Contributions are
/// scaled the same way and merged by feature, so they still add up to the
/// score (up to a constant shared by all candidates, which a softmax ignores).
fn ensemble_scores(
    model: &RankModel,
    extra: &[RankModel],
    feats: &[ServiceFeatures],
) -> (Vec<f64>, Vec<Vec<(String, f64)>>) {
    let single = |m: &RankModel| -> (Vec<f64>, Vec<Vec<(String, f64)>>) {
        feats
            .iter()
            .map(|f| {
                let s = m.score(&f.values);
                (s.logit, s.contributions)
            })
            .unzip()
    };
    if extra.is_empty() {
        return single(model);
    }
    let n = feats.len();
    let mut total = vec![0.0; n];
    let mut merged: Vec<BTreeMap<String, f64>> = vec![BTreeMap::new(); n];
    for m in std::iter::once(model).chain(extra) {
        let (logits, contributions) = single(m);
        #[allow(clippy::cast_precision_loss)]
        let mean = logits.iter().sum::<f64>() / n.max(1) as f64;
        #[allow(clippy::cast_precision_loss)]
        let var = logits.iter().map(|l| (l - mean) * (l - mean)).sum::<f64>() / n.max(1) as f64;
        let sd = if var.sqrt() > 1e-12 { var.sqrt() } else { 1.0 };
        for i in 0..n {
            total[i] += (logits[i] - mean) / sd;
            for (name, c) in &contributions[i] {
                *merged[i].entry(name.clone()).or_insert(0.0) += c / sd;
            }
        }
    }
    let contributions = merged.into_iter().map(|m| m.into_iter().collect()).collect();
    (total, contributions)
}

/// Computes the feature vectors [`analyze`] scores with [`Method::Etio`],
/// for model training. Uses the same code path as [`analyze`].
///
/// # Errors
/// Same conditions as [`analyze`].
pub fn service_features(input: &RcaInput, cfg: &RcaConfig) -> Result<Vec<ServiceFeatures>, RcaError> {
    let cfg = RcaConfig { method: Method::Etio, ..cfg.clone() };
    Ok(prepare(input, &cfg)?.feats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use etio_core::rng::Rng;

    /// A toy system: frontend -> cart -> redis, frontend -> catalog.
    /// A CPU fault in `cart` raises its CPU and latency, and the frontend's latency.
    #[allow(clippy::cast_precision_loss)]
    fn incident(seed: u64) -> RcaInput {
        let mut rng = Rng::seed_from_u64(seed);
        let n = 600;
        let t0 = 300;
        let times: Vec<f64> = (0..n).map(|i| 1_700_000_000.0 + i as f64).collect();
        let mut series = Vec::new();
        let mut make = |svc: &str, name: &str, base: f64, sd: f64, delta: f64, lag: usize| {
            let values = (0..n).map(|i| base + rng.normal(0.0, sd) + if i >= t0 + lag { delta } else { 0.0 }).collect();
            series.push(SeriesInput::new(svc, name, values));
        };
        make("cart", "cpu", 0.3, 0.02, 0.5, 0);
        make("cart", "latency", 20.0, 2.0, 30.0, 1);
        make("frontend", "latency", 80.0, 6.0, 35.0, 2);
        make("frontend", "cpu", 0.5, 0.03, 0.0, 0);
        make("redis", "cpu", 0.1, 0.01, 0.0, 0);
        make("catalog", "latency", 10.0, 1.0, 0.0, 0);
        let graph = ServiceGraph::from_edges([("frontend", "cart"), ("cart", "redis"), ("frontend", "catalog")]);
        RcaInput { times, anomaly_time: 1_700_000_000.0 + t0 as f64, series, graph: Some(graph), exclude: vec![] }
    }

    #[test]
    fn every_method_finds_the_obvious_root_cause() {
        let input = incident(1);
        for method in Method::ALL {
            let cfg = RcaConfig { method, ..RcaConfig::default() };
            let r = analyze(&input, &cfg).unwrap();
            assert_eq!(r.ranking[0].service, "cart", "method {method:?}: {:?}", r.services());
        }
    }

    #[test]
    fn etio_result_is_explained() {
        let r = analyze(&incident(2), &RcaConfig::default()).unwrap();
        let top = &r.ranking[0];
        assert_eq!(top.rank, 1);
        assert!(top.probability > 0.5, "{}", top.probability);
        assert!(!top.reasons.is_empty());
        assert_eq!(top.signals[0].name, "cpu");
        let sum: f64 = r.ranking.iter().map(|x| x.probability).sum();
        assert!((sum - 1.0).abs() < 1e-9);
        // Contributions add up to the score up to a constant shared by every
        // candidate: differences between candidates are exact.
        let total = |r: &RankedService| r.contributions.iter().map(|(_, c)| c).sum::<f64>();
        let (a, b) = (&r.ranking[0], &r.ranking[1]);
        assert!(((total(a) - total(b)) - (a.score - b.score)).abs() < 1e-9);
    }

    #[test]
    fn exclusions_are_never_blamed() {
        let mut input = incident(3);
        input.exclude = vec!["cart".to_owned()];
        let r = analyze(&input, &RcaConfig::default()).unwrap();
        assert!(r.rank_of("cart").is_none());
    }

    #[test]
    fn rejects_malformed_input() {
        let mut input = incident(4);
        input.series[0].values.pop();
        assert!(matches!(analyze(&input, &RcaConfig::default()), Err(RcaError::Length { .. })));

        let mut input = incident(4);
        input.anomaly_time = input.times[0];
        assert!(matches!(analyze(&input, &RcaConfig::default()), Err(RcaError::AnomalyTime(_))));

        let mut input = incident(4);
        input.times.swap(0, 1);
        assert_eq!(analyze(&input, &RcaConfig::default()), Err(RcaError::BadTimes));

        let mut input = incident(4);
        input.series.clear();
        assert_eq!(analyze(&input, &RcaConfig::default()), Err(RcaError::NoSeries));
    }

    #[test]
    fn analysis_is_deterministic() {
        let input = incident(5);
        let a = analyze(&input, &RcaConfig::default()).unwrap();
        let b = analyze(&input, &RcaConfig::default()).unwrap();
        assert_eq!(a, b);
        let feats = service_features(&input, &RcaConfig::default()).unwrap();
        assert_eq!(feats.len(), 4);
        assert!(feats.iter().all(|f| f.values.len() == FEATURE_NAMES.len()));
    }

    #[test]
    fn windows_limit_the_periods() {
        let input = incident(6);
        let cfg = RcaConfig { reference_window_s: Some(60.0), abnormal_window_s: Some(120.0), ..RcaConfig::default() };
        let r = analyze(&input, &cfg).unwrap();
        assert!((r.reference.0 - (input.anomaly_time - 60.0)).abs() < 1e-9);
        assert!((r.abnormal.1 - (input.anomaly_time + 119.0)).abs() < 1e-9);
        assert_eq!(r.ranking[0].service, "cart");
    }
}
