//! Engine configuration.

use std::time::Duration;

use etio_analysis::detect::DetectorConfig;
use etio_analysis::rca::RcaConfig;
use etio_core::Resolution;
use etio_core::time::serde_duration;
use etio_pipeline::logs::DrainConfig;
use serde::{Deserialize, Serialize};

/// When incidents open, how long they stay open, and how they are analysed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IncidentConfig {
    /// Minimum surprise (`−log10 p`) of the *current* value for an anomalous
    /// series to count: a series whose latest value is unremarkable does not
    /// keep an incident alive (0 disables the filter).
    pub min_surprise: f64,
    /// An anomaly counts only once it has lasted this many windows. A single
    /// extreme value is noise; incidents persist. Under the null hypothesis the
    /// probability of `k` consecutive extreme-value alarms decays like `risk^k`,
    /// which keeps the incident false-alarm rate low even with many series.
    pub confirm_windows: u32,
    /// Number of simultaneously anomalous services that opens an incident
    /// (an anomalous entry point opens one on its own).
    pub min_services: usize,
    /// A single service whose confirmed anomaly reaches this surprise opens
    /// an incident on its own (a service at 100% errors is an incident even
    /// before its callers show it).
    pub severe_surprise: f64,
    /// Wait after opening before the first root-cause analysis, so that
    /// evidence accumulates.
    #[serde(with = "serde_duration")]
    pub rca_delay: Duration,
    /// Interval between re-analyses while the incident is open.
    #[serde(with = "serde_duration")]
    pub rca_interval: Duration,
    /// Quiet period after which an incident resolves.
    #[serde(with = "serde_duration")]
    pub resolve_after: Duration,
    /// Reference period analysed before the incident start.
    #[serde(with = "serde_duration")]
    pub reference: Duration,
    /// No incident opens during this period after the first data, while
    /// detectors learn their baselines and tail models. `None` derives it
    /// from the detector settings (baseline plus calibration windows).
    #[serde(default, with = "opt_duration")]
    pub warmup: Option<Duration>,
    /// Services that are user-facing. Empty: inferred from trace roots.
    pub entry_points: Vec<String>,
    /// Services never blamed (load generators, meshes).
    pub exclude: Vec<String>,
}

impl Default for IncidentConfig {
    fn default() -> Self {
        Self {
            min_surprise: 1.0,
            confirm_windows: 3,
            min_services: 2,
            severe_surprise: 8.0,
            rca_delay: Duration::from_secs(30),
            rca_interval: Duration::from_secs(60),
            resolve_after: Duration::from_secs(180),
            reference: Duration::from_secs(20 * 60),
            warmup: None,
            entry_points: Vec::new(),
            exclude: Vec::new(),
        }
    }
}

/// Serde adapter for optional human-readable durations.
mod opt_duration {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => etio_core::time::serde_duration::serialize(d, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
        #[derive(Deserialize)]
        struct Wrap(#[serde(with = "etio_core::time::serde_duration")] Duration);
        Ok(Option::<Wrap>::deserialize(d)?.map(|w| w.0))
    }
}

/// Resource bounds of the engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    /// Maximum number of tracked series.
    pub max_series: usize,
    /// Maximum spans buffered while traces assemble.
    pub max_buffered_spans: usize,
    /// Maximum spans of a single trace before it is analysed in chunks.
    pub max_spans_per_trace: usize,
    /// Maximum distinct strings (services, operations, metric names).
    pub max_symbols: usize,
    /// Maximum open windows ahead of the oldest one (future timestamps beyond
    /// that are rejected as clock errors).
    pub max_open_windows: usize,
    /// Maximum cumulative-counter streams tracked.
    pub max_streams: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_series: 100_000,
            max_buffered_spans: 2_000_000,
            max_spans_per_trace: 10_000,
            max_symbols: 1 << 20,
            max_open_windows: 360,
            max_streams: 500_000,
        }
    }
}

/// Everything the engine needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineConfig {
    /// Aggregation window width.
    #[serde(with = "serde_duration")]
    pub resolution: Duration,
    /// How long after its end a window waits for late data before it closes.
    #[serde(with = "serde_duration")]
    pub lateness: Duration,
    /// Idle time after which a trace is considered complete.
    #[serde(with = "serde_duration")]
    pub trace_timeout: Duration,
    /// History kept in memory for analysis.
    #[serde(with = "serde_duration")]
    pub retention: Duration,
    /// Log templates first seen within this horizon are novel.
    #[serde(with = "serde_duration")]
    pub novelty_horizon: Duration,
    /// No template is novel during this warm-up after start.
    #[serde(with = "serde_duration")]
    pub novelty_warmup: Duration,
    /// Per-series detector.
    pub detector: DetectorConfig,
    /// Incident policy.
    pub incident: IncidentConfig,
    /// Root-cause analysis.
    pub rca: RcaConfig,
    /// Log template mining.
    pub drain: DrainConfig,
    /// Resource bounds.
    pub limits: Limits,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            resolution: Duration::from_secs(10),
            lateness: Duration::from_secs(20),
            trace_timeout: Duration::from_secs(8),
            retention: Duration::from_secs(2 * 3600),
            novelty_horizon: Duration::from_secs(3600),
            novelty_warmup: Duration::from_secs(600),
            detector: DetectorConfig::default(),
            incident: IncidentConfig::default(),
            rca: RcaConfig::default(),
            drain: DrainConfig::default(),
            limits: Limits::default(),
        }
    }
}

/// An inconsistent configuration.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    /// A duration that must be positive is zero.
    #[error("`{0}` must be positive")]
    NonPositive(&'static str),
    /// Traces would complete after their windows closed.
    #[error(
        "`lateness` ({lateness:?}) must exceed `trace_timeout` ({timeout:?}) plus one `resolution`, or trace data would arrive after its window closed"
    )]
    LatenessTooShort {
        /// Configured lateness.
        lateness: Duration,
        /// Configured trace timeout.
        timeout: Duration,
    },
    /// The retention cannot hold the analysis window.
    #[error("`retention` must be at least `incident.reference` plus 10 minutes")]
    RetentionTooShort,
}

impl EngineConfig {
    /// Checks cross-field constraints.
    ///
    /// # Errors
    /// Returns the first violated constraint.
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (name, d) in
            [("resolution", self.resolution), ("trace_timeout", self.trace_timeout), ("retention", self.retention)]
        {
            if d.is_zero() {
                return Err(ConfigError::NonPositive(name));
            }
        }
        if self.lateness < self.trace_timeout + self.resolution {
            return Err(ConfigError::LatenessTooShort { lateness: self.lateness, timeout: self.trace_timeout });
        }
        if self.retention < self.incident.reference + Duration::from_secs(600) {
            return Err(ConfigError::RetentionTooShort);
        }
        Ok(())
    }

    /// The window width as a [`Resolution`].
    ///
    /// # Panics
    /// Panics on a zero resolution; call [`EngineConfig::validate`] first.
    #[must_use]
    pub fn resolution(&self) -> Resolution {
        Resolution::from_duration(self.resolution).unwrap_or_else(|_| Resolution::from_secs(1))
    }

    /// Number of windows spanned by a duration (at least one).
    #[must_use]
    pub fn windows(&self, d: Duration) -> usize {
        self.resolution().windows_in(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        assert_eq!(EngineConfig::default().validate(), Ok(()));
    }

    #[test]
    fn rejects_lateness_shorter_than_trace_timeout() {
        let cfg = EngineConfig { lateness: Duration::from_secs(5), ..EngineConfig::default() };
        assert!(matches!(cfg.validate(), Err(ConfigError::LatenessTooShort { .. })));
    }

    #[test]
    fn rejects_short_retention() {
        let cfg = EngineConfig { retention: Duration::from_secs(60), ..EngineConfig::default() };
        assert_eq!(cfg.validate(), Err(ConfigError::RetentionTooShort));
    }

    #[test]
    fn parses_from_json_with_human_durations() {
        let cfg: EngineConfig = serde_json::from_str(
            r#"{"resolution": "5s", "lateness": "30s", "incident": {"resolve_after": "10m", "min_services": 1}}"#,
        )
        .unwrap();
        assert_eq!(cfg.resolution, Duration::from_secs(5));
        assert_eq!(cfg.incident.resolve_after, Duration::from_secs(600));
        assert_eq!(cfg.incident.min_services, 1);
        assert!(serde_json::from_str::<EngineConfig>(r#"{"resolutoin": "5s"}"#).is_err(), "typos are rejected");
    }
}
