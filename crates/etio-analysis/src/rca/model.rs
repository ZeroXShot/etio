//! The root-cause ranking model.
//!
//! A deliberately simple model: a linear score over standardised features,
//! turned into a distribution over candidate services with a softmax. It is
//! trained listwise (the cross-entropy of the true root cause under that
//! softmax, i.e. ListNet top-one) by the evaluation harness and shipped as a
//! small JSON file.
//!
//! Why linear? Labelled incidents are scarce (hundreds, not millions), and a
//! ranking an on-call engineer cannot interrogate is a ranking they will not
//! trust. With a linear model every score decomposes exactly into per-feature
//! contributions, which is what the explanations shown to users are built from.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::features::{FEATURE_NAMES, FEATURE_SET_VERSION, feature_index};

/// File format identifier written into every model.
pub const MODEL_FORMAT: &str = "etio.rank-model/v1";

/// Errors raised when loading a model.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ModelError {
    /// The file declares another format.
    #[error("unsupported model format `{0}`, expected `{MODEL_FORMAT}`")]
    Format(String),
    /// The model was trained on another feature set.
    #[error("model uses feature set {found}, this build computes feature set {expected}")]
    FeatureSet {
        /// Feature set declared by the model.
        found: u32,
        /// Feature set computed by this build.
        expected: u32,
    },
    /// A feature name is unknown.
    #[error("model refers to unknown feature `{0}`")]
    UnknownFeature(String),
    /// Vectors have inconsistent lengths.
    #[error("model vectors have inconsistent lengths")]
    Shape,
    /// A parameter is NaN, infinite, or a non-positive scale.
    #[error("model parameter `{0}` is invalid")]
    Parameter(String),
}

/// A linear listwise ranking model.
///
/// Deserialisation always validates, so a model obtained from JSON is ready
/// to score. Code that edits the public fields must call
/// [`RankModel::validate`] before scoring.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RankModelFile", into = "RankModelFile")]
pub struct RankModel {
    /// Always [`MODEL_FORMAT`].
    pub format: String,
    /// Human-readable identifier.
    pub name: String,
    /// Feature set version the model was trained on.
    pub feature_set: u32,
    /// Names of the features used, in parameter order.
    pub features: Vec<String>,
    /// Standardisation offsets.
    pub mean: Vec<f64>,
    /// Standardisation scales (strictly positive).
    pub scale: Vec<f64>,
    /// Weights applied to standardised features.
    pub weights: Vec<f64>,
    /// Free-form provenance (training data, metrics, date).
    pub metadata: BTreeMap<String, String>,
    index: Vec<usize>,
}

/// On-disk representation of a [`RankModel`].
#[derive(Serialize, Deserialize)]
struct RankModelFile {
    format: String,
    name: String,
    feature_set: u32,
    features: Vec<String>,
    mean: Vec<f64>,
    scale: Vec<f64>,
    weights: Vec<f64>,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
}

impl TryFrom<RankModelFile> for RankModel {
    type Error = ModelError;
    fn try_from(f: RankModelFile) -> Result<Self, Self::Error> {
        let mut m = Self {
            format: f.format,
            name: f.name,
            feature_set: f.feature_set,
            features: f.features,
            mean: f.mean,
            scale: f.scale,
            weights: f.weights,
            metadata: f.metadata,
            index: Vec::new(),
        };
        m.validate()?;
        Ok(m)
    }
}

impl From<RankModel> for RankModelFile {
    fn from(m: RankModel) -> Self {
        Self {
            format: m.format,
            name: m.name,
            feature_set: m.feature_set,
            features: m.features,
            mean: m.mean,
            scale: m.scale,
            weights: m.weights,
            metadata: m.metadata,
        }
    }
}

/// A model score broken down by feature.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scored {
    /// Linear score (logit).
    pub logit: f64,
    /// `(feature, contribution)` pairs; contributions sum to the logit.
    pub contributions: Vec<(String, f64)>,
}

impl RankModel {
    /// The built-in model: hand-set weights encoding the reasoning of an
    /// experienced SRE, used until a trained model is supplied.
    #[must_use]
    pub fn heuristic() -> Self {
        let weights: [(&str, f64); 19] = [
            ("rel_max", 3.0),
            ("frac_silent", 2.0),
            ("log_max", 0.5),
            ("rank_score", 1.0),
            ("log_sustained", 0.3),
            ("log_resource", 0.4),
            ("log_trace_local", 0.4),
            ("log_logs", 0.1),
            ("frac_anomalous", 0.5),
            ("onset_lead", 0.5),
            ("has_onset", 0.2),
            ("walk", 1.5),
            ("callee_explained", -1.5),
            ("upstream_anomalous", 0.5),
            ("downstream_anomalous", -0.5),
            ("is_entry", -0.5),
            ("log_latency", 0.0),
            ("log_errors", 0.0),
            ("log_traffic", 0.0),
        ];
        let n = weights.len();
        let mut m = Self {
            format: MODEL_FORMAT.to_owned(),
            name: "heuristic".to_owned(),
            feature_set: FEATURE_SET_VERSION,
            features: weights.iter().map(|(f, _)| (*f).to_owned()).collect(),
            mean: vec![0.0; n],
            scale: vec![1.0; n],
            weights: weights.iter().map(|(_, w)| *w).collect(),
            metadata: BTreeMap::from([(
                "description".to_owned(),
                "hand-set weights; replace with a trained model".to_owned(),
            )]),
            index: Vec::new(),
        };
        m.index = m.features.iter().filter_map(|f| feature_index(f)).collect();
        m
    }

    /// The model shipped with Etio, trained on every case of the RCAEval
    /// benchmark (see its `metadata` for the training data and the
    /// cross-validated accuracy). Because it has seen RCAEval, it must not be
    /// used to *evaluate* on RCAEval: the evaluation harness reports
    /// out-of-fold results instead.
    #[must_use]
    pub fn bundled() -> Self {
        Self::from_json(include_str!("../../models/etio-rank-v1.json")).unwrap_or_else(|_| Self::heuristic())
    }

    /// Parses and validates a JSON model.
    ///
    /// # Errors
    /// Returns a [`ModelError`] if the JSON is malformed or inconsistent.
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        let file: RankModelFile = serde_json::from_str(json).map_err(|e| ModelError::Format(e.to_string()))?;
        Self::try_from(file)
    }

    /// Serialises the model to pretty JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| String::from("{}"))
    }

    /// Checks internal consistency and resolves feature indices.
    ///
    /// # Errors
    /// Returns a [`ModelError`] describing the first problem found.
    pub fn validate(&mut self) -> Result<(), ModelError> {
        if self.format != MODEL_FORMAT {
            return Err(ModelError::Format(self.format.clone()));
        }
        if self.feature_set != FEATURE_SET_VERSION {
            return Err(ModelError::FeatureSet { found: self.feature_set, expected: FEATURE_SET_VERSION });
        }
        let n = self.features.len();
        if self.mean.len() != n || self.scale.len() != n || self.weights.len() != n {
            return Err(ModelError::Shape);
        }
        let mut index = Vec::with_capacity(n);
        for (i, f) in self.features.iter().enumerate() {
            let idx = feature_index(f).ok_or_else(|| ModelError::UnknownFeature(f.clone()))?;
            if !self.mean[i].is_finite() {
                return Err(ModelError::Parameter(format!("mean[{f}]")));
            }
            if !(self.scale[i].is_finite() && self.scale[i] > 0.0) {
                return Err(ModelError::Parameter(format!("scale[{f}]")));
            }
            if !self.weights[i].is_finite() {
                return Err(ModelError::Parameter(format!("weights[{f}]")));
            }
            index.push(idx);
        }
        self.index = index;
        Ok(())
    }

    /// Scores one feature vector (in [`FEATURE_NAMES`] order).
    #[must_use]
    pub fn score(&self, features: &[f64]) -> Scored {
        debug_assert_eq!(features.len(), FEATURE_NAMES.len());
        debug_assert_eq!(self.index.len(), self.features.len(), "model used before validate()");
        let mut logit = 0.0;
        let mut contributions = Vec::with_capacity(self.index.len());
        for (i, &idx) in self.index.iter().enumerate() {
            let x = features[idx];
            let z = if x.is_finite() { (x - self.mean[i]) / self.scale[i] } else { 0.0 };
            let c = self.weights[i] * z;
            logit += c;
            contributions.push((self.features[i].clone(), c));
        }
        Scored { logit, contributions }
    }
}

/// Numerically stable softmax.
#[must_use]
pub fn softmax(logits: &[f64]) -> Vec<f64> {
    let max = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !max.is_finite() {
        #[allow(clippy::cast_precision_loss)]
        let u = 1.0 / logits.len().max(1) as f64;
        return vec![u; logits.len()];
    }
    let exps: Vec<f64> = logits.iter().map(|l| (l - max).exp()).collect();
    let sum: f64 = exps.iter().sum();
    exps.into_iter().map(|e| e / sum).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rca::features::N_FEATURES;

    #[test]
    fn heuristic_model_is_valid_and_round_trips() {
        let m = RankModel::heuristic();
        let json = m.to_json();
        let back = RankModel::from_json(&json).unwrap();
        assert_eq!(back.features, m.features);
        let x = vec![1.0; N_FEATURES];
        let a = m.score(&x);
        let b = back.score(&x);
        assert!((a.logit - b.logit).abs() < 1e-12);
        let sum: f64 = a.contributions.iter().map(|(_, c)| c).sum();
        assert!((sum - a.logit).abs() < 1e-12);
    }

    #[test]
    fn bundled_model_loads() {
        let m = RankModel::bundled();
        assert_ne!(m.name, "heuristic", "the bundled model file must parse");
        assert!(m.metadata.contains_key("cv_avg5_learned"));
    }

    #[test]
    fn rejects_inconsistent_models() {
        let mut m = RankModel::heuristic();
        m.weights.pop();
        assert_eq!(m.validate(), Err(ModelError::Shape));

        let mut m = RankModel::heuristic();
        m.features[0] = "made_up".to_owned();
        assert_eq!(m.validate(), Err(ModelError::UnknownFeature("made_up".to_owned())));

        let mut m = RankModel::heuristic();
        m.scale[0] = 0.0;
        assert!(matches!(m.validate(), Err(ModelError::Parameter(_))));

        let mut m = RankModel::heuristic();
        m.feature_set = 99;
        assert!(matches!(m.validate(), Err(ModelError::FeatureSet { .. })));

        assert!(RankModel::from_json("{\"format\": \"nope\"}").is_err());
        // Deserialising through serde validates too.
        let mut json: serde_json::Value = serde_json::from_str(&RankModel::heuristic().to_json()).unwrap();
        json["scale"][0] = serde_json::json!(-1.0);
        assert!(serde_json::from_value::<RankModel>(json).is_err());
    }

    #[test]
    fn softmax_is_stable() {
        let p = softmax(&[1000.0, 1000.0, -1000.0]);
        assert!((p[0] - 0.5).abs() < 1e-12);
        assert!(p[2] < 1e-300);
        let u = softmax(&[f64::NEG_INFINITY, f64::NEG_INFINITY]);
        assert!((u[0] - 0.5).abs() < 1e-12);
        assert!(softmax(&[]).is_empty());
    }
}
