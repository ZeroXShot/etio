//! Service-level features for root-cause ranking.
//!
//! Each candidate service is described by a fixed vector of features that
//! capture the evidence an SRE would weigh: how anomalous the service is
//! compared with the others, whether the anomaly is *local* (resources,
//! self-time, errors originating there) or could be inherited, whether it
//! started first, and whether the dependency graph says it explains the
//! anomalies of its callers.
//!
//! Features are scale-free (logs of standardised scores, ratios, ranks,
//! fractions) so that a model trained on one system transfers to another.
//! The list is versioned by [`FEATURE_SET_VERSION`]: models declare the
//! names they use, and a model referring to unknown features is rejected.

use std::collections::BTreeMap;

use etio_core::SignalCategory;
use serde::{Deserialize, Serialize};

use super::score::SeriesScore;
use crate::graph::{ServiceGraph, WalkConfig, anomaly_random_walk};

/// Version of the feature definitions below.
pub const FEATURE_SET_VERSION: u32 = 2;

/// Canonical feature names, in vector order.
pub const FEATURE_NAMES: [&str; 20] = [
    "log_max",
    "rel_max",
    "rank_score",
    "log_sustained",
    "log_resource",
    "log_latency",
    "log_errors",
    "log_traffic",
    "log_logs",
    "log_trace_local",
    "frac_anomalous",
    "onset_lead",
    "has_onset",
    "walk",
    "callee_explained",
    "upstream_anomalous",
    "downstream_anomalous",
    "is_entry",
    "in_graph",
    "frac_silent",
];

/// Number of features.
pub const N_FEATURES: usize = FEATURE_NAMES.len();

/// Index of a feature by name.
#[must_use]
pub fn feature_index(name: &str) -> Option<usize> {
    FEATURE_NAMES.iter().position(|n| *n == name)
}

/// A scored series, tagged with its service and category.
#[derive(Clone, Debug)]
pub struct ScoredSeries<'a> {
    /// Owning service.
    pub service: &'a str,
    /// Series name.
    pub name: &'a str,
    /// Category.
    pub category: SignalCategory,
    /// Scores.
    pub score: SeriesScore,
}

/// Aggregated per-service evidence plus its feature vector.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServiceFeatures {
    /// Service name.
    pub service: String,
    /// Feature values in [`FEATURE_NAMES`] order.
    pub values: Vec<f64>,
    /// Maximum harmful score over the service's series.
    pub max_score: f64,
    /// Earliest onset among the service's anomalous series, seconds from t0.
    pub onset_s: Option<f64>,
}

impl ServiceFeatures {
    /// Features as a name → value map.
    #[must_use]
    pub fn as_map(&self) -> BTreeMap<String, f64> {
        FEATURE_NAMES.iter().zip(&self.values).map(|(k, v)| ((*k).to_owned(), *v)).collect()
    }
}

fn category_group(c: SignalCategory) -> usize {
    use SignalCategory as C;
    match c {
        C::Cpu | C::Memory | C::Disk | C::Network | C::Connections | C::Runtime => 0,
        C::Latency => 1,
        C::Errors => 2,
        C::Traffic => 3,
        C::Logs => 4,
        C::SelfTime | C::ErrorOrigin => 5,
        C::Other => 6,
    }
}

#[derive(Clone)]
struct Acc {
    /// Signed maximum score (baseline conventions can be negative).
    max: f64,
    sustained: f64,
    group_max: [f64; 7],
    n: usize,
    n_anomalous: usize,
    onset: Option<f64>,
}

impl Default for Acc {
    fn default() -> Self {
        Self { max: f64::NEG_INFINITY, sustained: 0.0, group_max: [0.0; 7], n: 0, n_anomalous: 0, onset: None }
    }
}

/// Computes features for every service that owns at least one scored series
/// or appears in the graph.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn compute(
    scored: &[ScoredSeries<'_>],
    silent: &BTreeMap<&str, (usize, usize)>,
    graph: Option<&ServiceGraph>,
    threshold: f64,
    walk: &WalkConfig,
) -> Vec<ServiceFeatures> {
    let mut acc: BTreeMap<&str, Acc> = BTreeMap::new();
    // Services whose every series went silent still are candidates.
    for (svc, &(n_silent, _)) in silent {
        if n_silent > 0 {
            acc.entry(svc).or_default();
        }
    }
    for s in scored {
        let a = acc.entry(s.service).or_default();
        let sc = &s.score;
        let g = category_group(s.category);
        a.group_max[g] = a.group_max[g].max(sc.max);
        // Request volume describes the workload, not the health of the
        // service: a traffic change is almost always a consequence (callers
        // failing, retries) and is kept out of the evidence ranking. It still
        // has its own feature.
        if !is_evidence(s.category) {
            continue;
        }
        a.max = a.max.max(sc.max);
        a.sustained = a.sustained.max(sc.sustained);
        a.n += 1;
        if sc.max > threshold {
            a.n_anomalous += 1;
            if let Some(o) = sc.onset_s {
                a.onset = Some(a.onset.map_or(o, |cur: f64| cur.min(o)));
            }
        }
    }
    if let Some(g) = graph {
        for name in g.names() {
            acc.entry(name.as_str()).or_default();
        }
    }
    // A candidate without scored series (known only from the graph) is neutral.
    for a in acc.values_mut() {
        if a.n == 0 {
            a.max = 0.0;
        }
    }

    let services: Vec<&str> = acc.keys().copied().collect();
    let n = services.len();
    let global_max = acc.values().map(|a| a.max).fold(0.0, f64::max);
    let pos = |x: f64| x.max(0.0);

    // Rank by max score (ties share the better rank).
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| acc[services[b]].max.total_cmp(&acc[services[a]].max));
    let mut rank = vec![0usize; n];
    for (pos, &i) in order.iter().enumerate() {
        rank[i] = if pos > 0 && acc[services[order[pos - 1]]].max.total_cmp(&acc[services[i]].max).is_eq() {
            rank[order[pos - 1]]
        } else {
            pos
        };
    }

    // Onset lead: 1 for the earliest anomalous service, 0 for the latest.
    let onsets: Vec<f64> = acc.values().filter_map(|a| a.onset).collect();
    let earliest = onsets.iter().copied().fold(f64::INFINITY, f64::min);
    let latest = onsets.iter().copied().fold(f64::NEG_INFINITY, f64::max);

    // Graph-derived features.
    let mut walk_score = vec![0.0; n];
    let mut callee_explained = vec![0.0; n];
    let mut upstream = vec![0.0; n];
    let mut downstream = vec![0.0; n];
    let mut is_entry = vec![0.0; n];
    let mut in_graph = vec![0.0; n];
    if let Some(g) = graph.filter(|g| !g.is_empty()) {
        let node_score: Vec<f64> =
            g.names().iter().map(|name| acc.get(name.as_str()).map_or(0.0, |a| relative(a.max, global_max))).collect();
        let pi = anomaly_random_walk(g, &node_score, walk);
        let pi_max = pi.iter().copied().fold(0.0, f64::max);
        let anomalous = |node: usize| acc.get(g.name(node)).is_some_and(|a| a.max > threshold);
        for (i, svc) in services.iter().enumerate() {
            let Some(node) = g.node(svc) else { continue };
            in_graph[i] = 1.0;
            walk_score[i] = if pi_max > 0.0 { pi[node] / pi_max } else { 0.0 };
            is_entry[i] = if g.callers(node).is_empty() { 1.0 } else { 0.0 };
            let own_latency = acc[svc].group_max[1].max(acc[svc].group_max[2]);
            if own_latency > threshold {
                callee_explained[i] = g
                    .callees(node)
                    .iter()
                    .filter_map(|&c| acc.get(g.name(c)))
                    .map(|a| (a.max / own_latency).min(1.0))
                    .fold(0.0, f64::max);
            }
            let ancestors = g.ancestors(node);
            if !ancestors.is_empty() {
                upstream[i] = ancestors.iter().filter(|&&a| anomalous(a)).count() as f64 / ancestors.len() as f64;
            }
            let descendants = g.descendants(node);
            if !descendants.is_empty() {
                downstream[i] = descendants.iter().filter(|&&d| anomalous(d)).count() as f64 / descendants.len() as f64;
            }
        }
    }

    services
        .iter()
        .enumerate()
        .map(|(i, svc)| {
            let a = &acc[svc];
            let onset_lead = match a.onset {
                Some(o) if latest > earliest => (latest - o) / (latest - earliest),
                Some(_) => 1.0,
                None => 0.0,
            };
            let rank_score = if n > 1 { 1.0 - rank[i] as f64 / (n - 1) as f64 } else { 1.0 };
            let frac = if a.n > 0 { a.n_anomalous as f64 / a.n as f64 } else { 0.0 };
            let values = vec![
                pos(a.max).ln_1p(),
                relative(a.max, global_max),
                rank_score,
                a.sustained.max(0.0).ln_1p(),
                pos(a.group_max[0]).ln_1p(),
                pos(a.group_max[1]).ln_1p(),
                pos(a.group_max[2]).ln_1p(),
                pos(a.group_max[3]).ln_1p(),
                pos(a.group_max[4]).ln_1p(),
                pos(a.group_max[5]).ln_1p(),
                frac,
                onset_lead,
                if a.onset.is_some() { 1.0 } else { 0.0 },
                walk_score[i],
                callee_explained[i],
                upstream[i],
                downstream[i],
                is_entry[i],
                in_graph[i],
                silent.get(svc).map_or(0.0, |&(s, total)| if total > 0 { s as f64 / total as f64 } else { 0.0 }),
            ];
            debug_assert_eq!(values.len(), N_FEATURES);
            ServiceFeatures { service: (*svc).to_owned(), values, max_score: a.max, onset_s: a.onset }
        })
        .collect()
}

/// Whether a category is evidence about the health of its service.
fn is_evidence(c: SignalCategory) -> bool {
    !matches!(c, SignalCategory::Traffic)
}

fn relative(x: f64, max: f64) -> f64 {
    if max > 0.0 { (x / max).clamp(0.0, 1.0) } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(max: f64, onset: Option<f64>) -> SeriesScore {
        SeriesScore {
            max,
            sustained: max / 2.0,
            shift: max / 2.0,
            onset_s: onset,
            reference_median: 0.0,
            scale: 1.0,
            peak_value: max,
            peak_offset_s: 1.0,
        }
    }

    fn get(fs: &[ServiceFeatures], svc: &str, feat: &str) -> f64 {
        let f = fs.iter().find(|f| f.service == svc).unwrap();
        f.values[feature_index(feat).unwrap()]
    }

    #[test]
    fn features_capture_local_and_inherited_evidence() {
        let graph = ServiceGraph::from_edges([("frontend", "cart"), ("cart", "redis")]);
        let scored = vec![
            ScoredSeries {
                service: "frontend",
                name: "latency",
                category: SignalCategory::Latency,
                score: score(8.0, Some(3.0)),
            },
            ScoredSeries {
                service: "cart",
                name: "latency",
                category: SignalCategory::Latency,
                score: score(9.0, Some(1.0)),
            },
            ScoredSeries { service: "cart", name: "cpu", category: SignalCategory::Cpu, score: score(20.0, Some(0.0)) },
            ScoredSeries { service: "redis", name: "cpu", category: SignalCategory::Cpu, score: score(0.5, None) },
        ];
        let fs = compute(&scored, &BTreeMap::new(), Some(&graph), 3.0, &WalkConfig::default());
        assert_eq!(fs.len(), 3);
        assert!((get(&fs, "cart", "rel_max") - 1.0).abs() < 1e-12);
        assert!((get(&fs, "cart", "rank_score") - 1.0).abs() < 1e-12);
        assert!((get(&fs, "cart", "onset_lead") - 1.0).abs() < 1e-12);
        assert!(get(&fs, "frontend", "onset_lead").abs() < 1e-12);
        assert!(get(&fs, "cart", "log_resource") > 3.0);
        // frontend's latency is fully explained by its anomalous callee.
        assert!((get(&fs, "frontend", "callee_explained") - 1.0).abs() < 1e-12);
        assert!(get(&fs, "cart", "callee_explained") < 0.1);
        assert!((get(&fs, "cart", "upstream_anomalous") - 1.0).abs() < 1e-12);
        assert!(get(&fs, "cart", "downstream_anomalous").abs() < 1e-12);
        assert!((get(&fs, "frontend", "is_entry") - 1.0).abs() < 1e-12);
        assert!((get(&fs, "cart", "walk") - 1.0).abs() < 1e-12);
    }

    #[test]
    fn works_without_a_graph() {
        let scored = vec![
            ScoredSeries { service: "a", name: "cpu", category: SignalCategory::Cpu, score: score(5.0, None) },
            ScoredSeries { service: "b", name: "cpu", category: SignalCategory::Cpu, score: score(5.0, None) },
        ];
        let fs = compute(&scored, &BTreeMap::from([("c", (2, 2))]), None, 3.0, &WalkConfig::default());
        assert_eq!(fs.len(), 3, "a service that went silent is a candidate");
        assert!((get(&fs, "c", "frac_silent") - 1.0).abs() < 1e-12);
        // Ties share the top rank.
        assert!((get(&fs, "a", "rank_score") - 1.0).abs() < 1e-12);
        assert!((get(&fs, "b", "rank_score") - 1.0).abs() < 1e-12);
        assert!(get(&fs, "a", "in_graph").abs() < 1e-12);
    }

    #[test]
    fn names_are_unique() {
        let mut names = FEATURE_NAMES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), N_FEATURES);
    }
}
