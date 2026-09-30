//! Vocabulary for describing telemetry series.
//!
//! Every series the engine tracks is classified into a [`SignalCategory`]. The
//! category decides which direction of change is harmful ([`Direction`]), the
//! noise floor used by detectors, and how the series contributes to root-cause
//! features (a CPU saturation is evidence of a *local* problem, a latency
//! increase may be inherited from a dependency).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Which direction of deviation from the baseline is considered harmful.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Increases are harmful (latency, errors, CPU).
    Up,
    /// Decreases are harmful (throughput of a healthy system, free memory).
    Down,
    /// Both directions are suspicious (traffic, most unknown metrics).
    Both,
}

impl Direction {
    /// Projects a signed deviation onto the harmful direction.
    ///
    /// Returns the deviation if it points in a harmful direction and zero otherwise.
    #[must_use]
    pub fn harmful(self, deviation: f64) -> f64 {
        match self {
            Self::Up => deviation.max(0.0),
            Self::Down => (-deviation).max(0.0),
            Self::Both => deviation.abs(),
        }
    }
}

/// The semantic class of a series.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalCategory {
    /// Request or operation duration.
    Latency,
    /// Error counts or ratios.
    Errors,
    /// Request throughput.
    Traffic,
    /// CPU usage or throttling.
    Cpu,
    /// Memory usage.
    Memory,
    /// Disk I/O.
    Disk,
    /// Network I/O, retransmissions, packet loss.
    Network,
    /// Sockets, file descriptors, connection pools.
    Connections,
    /// Garbage collection and runtime pauses.
    Runtime,
    /// Log-derived signals (error log rate, template frequencies).
    Logs,
    /// Time a service spends in its own code, excluding downstream calls.
    SelfTime,
    /// Share of failing requests whose error originates in this service.
    ErrorOrigin,
    /// Anything else.
    Other,
}

impl SignalCategory {
    /// All categories, in a stable order.
    pub const ALL: [Self; 13] = [
        Self::Latency,
        Self::Errors,
        Self::Traffic,
        Self::Cpu,
        Self::Memory,
        Self::Disk,
        Self::Network,
        Self::Connections,
        Self::Runtime,
        Self::Logs,
        Self::SelfTime,
        Self::ErrorOrigin,
        Self::Other,
    ];

    /// The default harmful direction for the category.
    #[must_use]
    pub const fn default_direction(self) -> Direction {
        match self {
            Self::Latency
            | Self::Errors
            | Self::Cpu
            | Self::Memory
            | Self::Disk
            | Self::Connections
            | Self::Runtime
            | Self::Logs
            | Self::SelfTime
            | Self::ErrorOrigin => Direction::Up,
            Self::Traffic | Self::Network | Self::Other => Direction::Both,
        }
    }

    /// Whether the category describes the health of the entity itself rather
    /// than symptoms that can be inherited from its dependencies.
    ///
    /// A latency increase in a caller is often inherited from a slow callee;
    /// a CPU saturation of the caller is not.
    #[must_use]
    pub const fn is_local(self) -> bool {
        matches!(
            self,
            Self::Cpu
                | Self::Memory
                | Self::Disk
                | Self::Network
                | Self::Connections
                | Self::Runtime
                | Self::SelfTime
                | Self::ErrorOrigin
        )
    }

    /// Stable lower-case name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Latency => "latency",
            Self::Errors => "errors",
            Self::Traffic => "traffic",
            Self::Cpu => "cpu",
            Self::Memory => "memory",
            Self::Disk => "disk",
            Self::Network => "network",
            Self::Connections => "connections",
            Self::Runtime => "runtime",
            Self::Logs => "logs",
            Self::SelfTime => "self_time",
            Self::ErrorOrigin => "error_origin",
            Self::Other => "other",
        }
    }

    /// Classifies a metric by its name using OpenTelemetry semantic
    /// conventions and common Prometheus naming patterns.
    ///
    /// The heuristics are deliberately conservative: an unknown metric is
    /// [`SignalCategory::Other`] and still takes part in detection.
    #[must_use]
    pub fn classify_metric_name(name: &str) -> Self {
        let n = name.to_ascii_lowercase();
        let has = |needle: &str| n.contains(needle);
        let token = |t: &str| n.split(|c: char| !c.is_ascii_alphanumeric()).any(|part| part == t);

        if has("self_time") || has("self-time") || has("selftime") {
            return Self::SelfTime;
        }
        if has("error_origin") {
            return Self::ErrorOrigin;
        }
        // Runtime pauses are durations too, so they must be matched before latency.
        if token("gc") || has("garbage") || has("goroutine") || has("thread") || has("pause") {
            return Self::Runtime;
        }
        if has("latency") || has("duration") || has("response_time") || token("rt") {
            return Self::Latency;
        }
        if has("error") || has("fail") || has("exception") || token("5xx") {
            return Self::Errors;
        }
        if token("log") || token("logs") {
            return Self::Logs;
        }
        if has("cpu") || has("throttl") {
            return Self::Cpu;
        }
        if token("mem") || has("memory") || has("heap") || token("rss") || has("oom") {
            return Self::Memory;
        }
        if has("disk") || has("filesystem") || token("fs") || token("io") || token("iops") {
            return Self::Disk;
        }
        if has("socket")
            || has("connection")
            || token("conn")
            || token("conns")
            || token("fd")
            || token("fds")
            || has("file_descriptor")
            || token("pool")
        {
            return Self::Connections;
        }
        if has("network")
            || token("net")
            || has("packet")
            || has("retrans")
            || has("receive")
            || has("transmit")
            || has("bytes_sent")
            || has("bytes_recv")
        {
            return Self::Network;
        }
        if has("request")
            || token("rate")
            || token("qps")
            || token("rps")
            || has("throughput")
            || token("load")
            || has("workload")
            || token("count")
            || token("total")
            || token("calls")
        {
            return Self::Traffic;
        }
        Self::Other
    }
}

/// Whether a metric describes the **host** a service runs on rather than the
/// service itself: OpenTelemetry's `system.*` host metrics (which the Python
/// and Java runtimes report from inside every container, with host-wide
/// values) and Prometheus node-exporter metrics (`node_*`).
///
/// Services sharing a host all report the same host metrics, so a host-wide
/// event (a noisy neighbour, a backup job) would look like every service
/// failing at once. Such series are kept as evidence but never open incidents.
#[must_use]
pub fn is_host_scoped(metric: &str) -> bool {
    metric.starts_with("system.") || metric.starts_with("host.") || metric.starts_with("node_")
}

impl fmt::Display for SignalCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing an unknown category name.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unknown signal category `{0}`")]
pub struct UnknownCategory(pub String);

impl FromStr for SignalCategory {
    type Err = UnknownCategory;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|c| c.as_str() == s).ok_or_else(|| UnknownCategory(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_metrics_are_recognised() {
        assert!(is_host_scoped("system.cpu.utilization"));
        assert!(is_host_scoped("node_cpu_seconds_total"));
        assert!(!is_host_scoped("process.cpu.utilization"));
        assert!(!is_host_scoped("container.cpu.utilization"));
        assert!(!is_host_scoped("http.server.request.duration"));
    }

    #[test]
    fn harmful_projection() {
        assert!((Direction::Up.harmful(2.0) - 2.0).abs() < 1e-12);
        assert!(Direction::Up.harmful(-2.0).abs() < 1e-12);
        assert!((Direction::Down.harmful(-3.0) - 3.0).abs() < 1e-12);
        assert!((Direction::Both.harmful(-3.0) - 3.0).abs() < 1e-12);
    }

    #[test]
    fn classifies_common_metric_names() {
        use SignalCategory as C;
        let cases = [
            ("http.server.request.duration", C::Latency),
            ("cartservice_latency-90", C::Latency),
            ("container_cpu_usage_seconds_total", C::Cpu),
            ("adservice_mem", C::Memory),
            ("jvm.memory.used", C::Memory),
            ("redis_diskio", C::Disk),
            ("frontend_socket", C::Connections),
            ("frontend_error", C::Errors),
            ("checkoutservice_workload", C::Traffic),
            ("ts-order-service_load", C::Traffic),
            ("jvm.gc.duration", C::Runtime),
            ("cache_hit_ratio", C::Other),
            ("process_open_fds", C::Connections),
            ("go_goroutines", C::Runtime),
            ("node_network_receive_bytes_total", C::Network),
            ("mystery_metric", C::Other),
        ];
        for (name, expected) in cases {
            assert_eq!(SignalCategory::classify_metric_name(name), expected, "{name}");
        }
    }

    #[test]
    fn round_trips_names() {
        for c in SignalCategory::ALL {
            assert_eq!(c.as_str().parse::<SignalCategory>(), Ok(c));
        }
        assert!("nope".parse::<SignalCategory>().is_err());
    }
}
