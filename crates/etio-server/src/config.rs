//! Server configuration.
//!
//! Configuration is layered: built-in defaults, then a TOML file, then
//! environment variables, then command-line flags. Environment overrides use
//! the `ETIO__` prefix and double underscores for nesting, so
//! `ETIO__LISTEN__API=0.0.0.0:8080` sets `listen.api` and
//! `ETIO__ENGINE__RESOLUTION=5s` sets `engine.resolution`.
//!
//! Unknown keys are errors (a typo must not silently fall back to a
//! default). Secrets are never part of the configuration itself: it only
//! names the files that hold them (Docker and Kubernetes secrets are files).

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use etio_core::time::serde_duration;
use etio_engine::EngineConfig;
use serde::{Deserialize, Serialize};

/// Where the server listens. A missing address disables that listener.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Listen {
    /// OTLP over gRPC.
    pub otlp_grpc: Option<SocketAddr>,
    /// OTLP over HTTP (protobuf and JSON).
    pub otlp_http: Option<SocketAddr>,
    /// REST API, server-sent events, Prometheus metrics and the web UI.
    pub api: Option<SocketAddr>,
}

impl Default for Listen {
    fn default() -> Self {
        Self {
            otlp_grpc: Some(SocketAddr::from(([0, 0, 0, 0], 4317))),
            otlp_http: Some(SocketAddr::from(([0, 0, 0, 0], 4318))),
            api: Some(SocketAddr::from(([0, 0, 0, 0], 7070))),
        }
    }
}

/// TLS for every listener.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// PEM certificate chain.
    pub cert_file: PathBuf,
    /// PEM private key.
    pub key_file: PathBuf,
    /// PEM CA bundle; when set, clients must present a certificate it signed (mTLS).
    #[serde(default)]
    pub client_ca_file: Option<PathBuf>,
}

/// Bearer-token authentication. Without token files, the corresponding
/// endpoints are open (suitable only behind a trusted network boundary).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// File holding the token required to send telemetry.
    pub ingest_token_file: Option<PathBuf>,
    /// File holding the token required to read the API.
    pub read_token_file: Option<PathBuf>,
}

/// Persistence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// State directory. Without it, the server keeps everything in memory.
    pub dir: Option<PathBuf>,
    /// How often the engine state is snapshotted.
    #[serde(with = "serde_duration")]
    pub snapshot_interval: Duration,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self { dir: None, snapshot_interval: Duration::from_secs(60) }
    }
}

/// Bounds on what clients can make the server do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IngestLimits {
    /// Largest accepted request body (compressed), bytes.
    pub max_request_bytes: usize,
    /// Largest accepted request after decompression, bytes (bounds decompression bombs).
    pub max_decompressed_bytes: usize,
    /// Batches waiting for the engine before clients are asked to back off.
    pub queue_batches: usize,
    /// How often the engine clock advances.
    #[serde(with = "serde_duration")]
    pub tick: Duration,
}

impl Default for IngestLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: 8 << 20,
            max_decompressed_bytes: 64 << 20,
            queue_batches: 256,
            tick: Duration::from_secs(1),
        }
    }
}

/// An HTTP endpoint notified of incidents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Webhook {
    /// Destination URL.
    pub url: String,
    /// Events to deliver: `opened`, `analyzed`, `resolved` (default: all).
    #[serde(default)]
    pub events: Vec<String>,
    /// File holding a secret used to sign deliveries (HMAC-SHA256).
    #[serde(default)]
    pub secret_file: Option<PathBuf>,
    /// Per-attempt timeout.
    #[serde(default = "default_webhook_timeout", with = "serde_duration")]
    pub timeout: Duration,
}

fn default_webhook_timeout() -> Duration {
    Duration::from_secs(5)
}

/// Outgoing notifications.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotifyConfig {
    /// Webhooks.
    pub webhooks: Vec<Webhook>,
}

/// Log output format.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    /// Human-readable.
    #[default]
    Pretty,
    /// One JSON object per line.
    Json,
}

/// Logging.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogConfig {
    /// `tracing` filter directive, e.g. `info` or `etio=debug,tower_http=info`.
    pub level: String,
    /// Output format.
    pub format: LogFormat,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self { level: "info".into(), format: LogFormat::Pretty }
    }
}

/// Web UI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    /// Directory with the built UI (`ui/dist`). Without it, no UI is served.
    pub dir: Option<PathBuf>,
}

/// What drives the engine clock.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockMode {
    /// The wall clock (production).
    #[default]
    Wall,
    /// The newest telemetry timestamp: replays of recorded telemetry and
    /// faster-than-real-time simulations (`etio sim run --speed 20`).
    Event,
}

/// The role of this server in a deployment.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// One node does everything.
    #[default]
    Standalone,
    /// Receives telemetry, aggregates it into window summaries and ships
    /// them to the cores. Runs no detection.
    Edge,
    /// Receives window summaries from the edges, merges them and runs
    /// detection, incidents and analysis. Receives no telemetry.
    Core,
}

/// Distributed deployment (see `docs/architecture.md`).
///
/// Telemetry of one trace must reach one edge (route by trace ID, e.g. with
/// the OpenTelemetry Collector's `loadbalancing` exporter); metrics and logs
/// may reach any edge. Every edge sends every summary to every core, so cores
/// are interchangeable replicas.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClusterConfig {
    /// Role of this server.
    pub role: Role,
    /// Edge: stable identity, unique in the cluster.
    pub edge_id: Option<String>,
    /// Edge: core endpoints, e.g. `http://etio-core-0:7071`.
    pub cores: Vec<String>,
    /// Core: where summaries are received.
    pub listen: SocketAddr,
    /// Core: how far (in event time) the newest edge may run ahead of an
    /// edge that has not reported a window before the window is closed
    /// without it.
    #[serde(with = "serde_duration")]
    pub deadline: Duration,
    /// Core: after start-up, how long to wait for edges to connect before
    /// closing windows.
    #[serde(with = "serde_duration")]
    pub grace: Duration,
    /// Core: how long an edge may stay silent before it stops holding windows back.
    #[serde(with = "serde_duration")]
    pub liveness: Duration,
    /// Edge: replay unacknowledged summaries after this long without progress.
    #[serde(with = "serde_duration")]
    pub retransmit: Duration,
    /// Edge: summaries retained while cores are unreachable (one per window).
    pub outbox_capacity: usize,
    /// File holding the token edges present to cores.
    pub token_file: Option<PathBuf>,
    /// Edge: PEM CA bundle to verify `https://` cores.
    pub ca_file: Option<PathBuf>,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            role: Role::Standalone,
            edge_id: None,
            cores: Vec::new(),
            listen: SocketAddr::from(([0, 0, 0, 0], 7071)),
            deadline: Duration::from_secs(30),
            grace: Duration::from_secs(15),
            liveness: Duration::from_secs(120),
            retransmit: Duration::from_secs(5),
            outbox_capacity: 4_320,
            token_file: None,
            ca_file: None,
        }
    }
}

impl ClusterConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: &str| Err(ConfigError::Invalid(format!("cluster: {m}")));
        match self.role {
            Role::Standalone => Ok(()),
            Role::Edge => {
                if self.edge_id.as_deref().is_none_or(|id| id.trim().is_empty()) {
                    return bad("an edge needs an edge_id");
                }
                if self.cores.is_empty() {
                    return bad("an edge needs at least one core");
                }
                for c in &self.cores {
                    if c.starts_with("https://") && self.ca_file.is_none() {
                        return bad("https cores need ca_file");
                    }
                    if !(c.starts_with("http://") || c.starts_with("https://")) {
                        return bad(&format!("core endpoint `{c}` must be an http(s) URL"));
                    }
                }
                if self.outbox_capacity == 0 || self.retransmit.is_zero() {
                    return bad("outbox_capacity and retransmit must be positive");
                }
                Ok(())
            }
            Role::Core => {
                if self.deadline.is_zero() || self.liveness < self.deadline {
                    return bad("deadline must be positive and liveness at least deadline");
                }
                Ok(())
            }
        }
    }
}

/// The complete server configuration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Listeners.
    pub listen: Listen,
    /// TLS for all listeners.
    pub tls: Option<TlsConfig>,
    /// Authentication.
    pub auth: AuthConfig,
    /// The analysis engine.
    pub engine: EngineConfig,
    /// Persistence.
    pub storage: StorageConfig,
    /// Ingestion bounds.
    pub limits: IngestLimits,
    /// Notifications.
    pub notify: NotifyConfig,
    /// Logging.
    pub log: LogConfig,
    /// Web UI.
    pub ui: UiConfig,
    /// Distributed deployment.
    pub cluster: ClusterConfig,
    /// What drives the engine clock.
    pub clock: ClockMode,
}

/// A configuration problem.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("reading {path}: {source}")]
    Read {
        /// File.
        path: PathBuf,
        /// Cause.
        source: std::io::Error,
    },
    /// The TOML is malformed or has unknown keys.
    #[error("invalid configuration: {0}")]
    Parse(String),
    /// An environment override is malformed.
    #[error("invalid environment override {0}")]
    Env(String),
    /// Cross-field validation failed.
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// Environment variable prefix for overrides.
pub const ENV_PREFIX: &str = "ETIO__";

impl ServerConfig {
    /// Loads the configuration: defaults, then `path` (if any), then
    /// environment overrides from `env` (pass `std::env::vars()`).
    ///
    /// # Errors
    /// Returns a [`ConfigError`] describing the first problem.
    pub fn load(path: Option<&Path>, env: impl IntoIterator<Item = (String, String)>) -> Result<Self, ConfigError> {
        let mut table = match path {
            Some(p) => {
                let text = std::fs::read_to_string(p).map_err(|source| ConfigError::Read { path: p.into(), source })?;
                text.parse::<toml::Table>().map_err(|e| ConfigError::Parse(e.to_string()))?
            }
            None => toml::Table::new(),
        };
        let mut overrides: Vec<(String, String)> = env.into_iter().filter(|(k, _)| k.starts_with(ENV_PREFIX)).collect();
        overrides.sort();
        for (key, value) in overrides {
            apply_override(&mut table, &key[ENV_PREFIX.len()..], &value).map_err(|()| ConfigError::Env(key.clone()))?;
        }
        let cfg: Self =
            toml::Value::Table(table).try_into().map_err(|e: toml::de::Error| ConfigError::Parse(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Checks cross-field constraints.
    ///
    /// # Errors
    /// Returns [`ConfigError::Invalid`] on the first violation.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.engine.validate().map_err(|e| ConfigError::Invalid(format!("engine: {e}")))?;
        if self.limits.max_request_bytes == 0 || self.limits.max_decompressed_bytes < self.limits.max_request_bytes {
            return Err(ConfigError::Invalid(
                "limits: max_decompressed_bytes must be at least max_request_bytes (and both positive)".into(),
            ));
        }
        if self.limits.queue_batches == 0 {
            return Err(ConfigError::Invalid("limits.queue_batches must be positive".into()));
        }
        if self.limits.tick.is_zero() {
            return Err(ConfigError::Invalid("limits.tick must be positive".into()));
        }
        for w in &self.notify.webhooks {
            if !(w.url.starts_with("http://") || w.url.starts_with("https://")) {
                return Err(ConfigError::Invalid(format!("webhook URL `{}` must be http(s)", w.url)));
            }
            for e in &w.events {
                if !matches!(e.as_str(), "opened" | "analyzed" | "resolved") {
                    return Err(ConfigError::Invalid(format!("unknown webhook event `{e}`")));
                }
            }
        }
        self.cluster.validate()?;
        if self.cluster.role != Role::Core && self.listen.otlp_grpc.is_none() && self.listen.otlp_http.is_none() {
            return Err(ConfigError::Invalid("at least one OTLP listener must be enabled".into()));
        }
        Ok(())
    }

    /// The default configuration as commented TOML.
    #[must_use]
    pub fn default_toml() -> String {
        let body = toml::to_string_pretty(&Self::default()).unwrap_or_default();
        format!(
            "# Etio server configuration (defaults).\n\
             # Every key can be overridden with an environment variable such as\n\
             # ETIO__LISTEN__API=0.0.0.0:8080 or ETIO__ENGINE__RESOLUTION=5s.\n\
             # Secrets are read from files (auth.*_file, tls.*, notify.webhooks[].secret_file).\n\n{body}"
        )
    }
}

/// Sets `path` (a `__`-separated, case-insensitive key path) to `value`,
/// interpreted as a TOML value when it parses as one (numbers, booleans,
/// arrays) and as a string otherwise.
fn apply_override(table: &mut toml::Table, path: &str, value: &str) -> Result<(), ()> {
    let parts: Vec<String> = path.split("__").map(str::to_ascii_lowercase).collect();
    if parts.iter().any(String::is_empty) {
        return Err(());
    }
    let parsed = format!("v = {value}")
        .parse::<toml::Table>()
        .ok()
        .and_then(|mut t| t.remove("v"))
        .unwrap_or_else(|| toml::Value::String(value.to_owned()));
    let (last, inner) = parts.split_last().ok_or(())?;
    let mut cur = table;
    for p in inner {
        cur = cur.entry(p.clone()).or_insert_with(|| toml::Value::Table(toml::Table::new())).as_table_mut().ok_or(())?;
    }
    cur.insert(last.clone(), parsed);
    Ok(())
}

/// A secret read from a file; never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Reads and trims a secret file.
    ///
    /// # Errors
    /// Fails if the file cannot be read or is empty.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let s = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading secret {}: {e}", path.display()))?
            .trim()
            .to_owned();
        anyhow::ensure!(!s.is_empty(), "secret file {} is empty", path.display());
        Ok(Self(s))
    }

    /// The secret value.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn defaults_are_valid_and_round_trip_through_toml() {
        let cfg = ServerConfig::load(None, env(&[])).unwrap();
        assert_eq!(cfg, ServerConfig::default());
        let text = ServerConfig::default_toml();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("etio.toml");
        std::fs::write(&path, text).unwrap();
        assert_eq!(ServerConfig::load(Some(&path), env(&[])).unwrap(), ServerConfig::default());
    }

    #[test]
    fn file_then_environment_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("etio.toml");
        std::fs::write(&path, "[listen]\napi = \"127.0.0.1:9000\"\n[engine]\nresolution = \"5s\"\n").unwrap();
        let cfg = ServerConfig::load(
            Some(&path),
            env(&[
                ("ETIO__LISTEN__API", "127.0.0.1:9999"),
                ("ETIO__ENGINE__INCIDENT__MIN_SERVICES", "3"),
                ("OTHER", "x"),
            ]),
        )
        .unwrap();
        assert_eq!(cfg.listen.api, Some("127.0.0.1:9999".parse().unwrap()));
        assert_eq!(cfg.engine.resolution, Duration::from_secs(5));
        assert_eq!(cfg.engine.incident.min_services, 3);
    }

    #[test]
    fn unknown_keys_and_bad_values_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("etio.toml");
        std::fs::write(&path, "[listen]\napii = \"127.0.0.1:9000\"\n").unwrap();
        assert!(matches!(ServerConfig::load(Some(&path), env(&[])), Err(ConfigError::Parse(_))));
        assert!(ServerConfig::load(None, env(&[("ETIO__ENGINE__LATENESS", "1s")])).is_err(), "cross-field validation");
        assert!(ServerConfig::load(None, env(&[("ETIO____X", "1")])).is_err());
        let hook = "[[notify.webhooks]]\nurl = \"ftp://x\"\n";
        std::fs::write(&path, hook).unwrap();
        assert!(matches!(ServerConfig::load(Some(&path), env(&[])), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn cluster_roles_are_validated() {
        let edge = env(&[("ETIO__CLUSTER__ROLE", "edge"), ("ETIO__CLUSTER__EDGE_ID", "edge-0")]);
        assert!(ServerConfig::load(None, edge.clone()).is_err(), "an edge without cores");
        let mut ok = edge.clone();
        ok.push(("ETIO__CLUSTER__CORES".into(), "[\"http://core-0:7071\"]".into()));
        let cfg = ServerConfig::load(None, ok).unwrap();
        assert_eq!(cfg.cluster.role, Role::Edge);
        assert_eq!(cfg.cluster.cores, vec!["http://core-0:7071".to_owned()]);
        let mut tls = edge;
        tls.push(("ETIO__CLUSTER__CORES".into(), "[\"https://core-0:7071\"]".into()));
        assert!(ServerConfig::load(None, tls).is_err(), "https without a CA bundle");
        let core = env(&[("ETIO__CLUSTER__ROLE", "core"), ("ETIO__CLUSTER__LIVENESS", "10s")]);
        assert!(ServerConfig::load(None, core).is_err(), "liveness below the deadline");
    }

    #[test]
    fn secrets_are_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "  s3cr3t\n").unwrap();
        let s = Secret::from_file(&path).unwrap();
        assert_eq!(s.expose(), "s3cr3t");
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
        std::fs::write(&path, "\n").unwrap();
        assert!(Secret::from_file(&path).is_err());
    }
}
