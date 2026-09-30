//! Event-time primitives.
//!
//! All engine logic runs on *event time*: the timestamps carried by the
//! telemetry itself, not the wall clock of the machine processing it. Time is
//! represented as signed nanoseconds since the Unix epoch, which is the OTLP
//! wire representation and covers roughly ±292 years.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

const NANOS_PER_SEC: i64 = 1_000_000_000;

/// A point in time, in nanoseconds since the Unix epoch (UTC).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

impl Timestamp {
    /// The Unix epoch.
    pub const EPOCH: Self = Self(0);
    /// The smallest representable timestamp.
    pub const MIN: Self = Self(i64::MIN);
    /// The largest representable timestamp.
    pub const MAX: Self = Self(i64::MAX);

    /// Builds a timestamp from whole seconds.
    #[must_use]
    pub const fn from_secs(secs: i64) -> Self {
        Self(secs.saturating_mul(NANOS_PER_SEC))
    }

    /// Builds a timestamp from milliseconds.
    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis.saturating_mul(1_000_000))
    }

    /// Builds a timestamp from fractional seconds, rounding to the nearest nanosecond.
    #[must_use]
    pub fn from_secs_f64(secs: f64) -> Self {
        #[allow(clippy::cast_possible_truncation)]
        Self((secs * 1e9).round() as i64)
    }

    /// Nanoseconds since the epoch.
    #[must_use]
    pub const fn as_nanos(self) -> i64 {
        self.0
    }

    /// Seconds since the epoch, truncated toward negative infinity.
    #[must_use]
    pub const fn as_secs(self) -> i64 {
        self.0.div_euclid(NANOS_PER_SEC)
    }

    /// Seconds since the epoch as a float.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / 1e9
    }

    /// Adds a duration, saturating at the representable range.
    #[must_use]
    pub fn saturating_add(self, d: Duration) -> Self {
        Self(self.0.saturating_add(duration_nanos(d)))
    }

    /// Subtracts a duration, saturating at the representable range.
    #[must_use]
    pub fn saturating_sub(self, d: Duration) -> Self {
        Self(self.0.saturating_sub(duration_nanos(d)))
    }

    /// Signed distance `self - earlier` in nanoseconds.
    #[must_use]
    pub const fn nanos_since(self, earlier: Self) -> i64 {
        self.0.saturating_sub(earlier.0)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        let per_sec = NANOS_PER_SEC.unsigned_abs();
        write!(f, "{sign}{}.{:09}s", abs / per_sec, abs % per_sec)
    }
}

/// Converts a [`Duration`] to signed nanoseconds, saturating at `i64::MAX`.
#[must_use]
pub fn duration_nanos(d: Duration) -> i64 {
    i64::try_from(d.as_nanos()).unwrap_or(i64::MAX)
}

/// The width of an aggregation window. Always strictly positive.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct Resolution(i64);

/// Error returned when constructing an invalid [`Resolution`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("resolution must be a positive number of nanoseconds, got {0}")]
pub struct InvalidResolution(pub i64);

impl Resolution {
    /// Creates a resolution from nanoseconds.
    ///
    /// # Errors
    /// Returns [`InvalidResolution`] if `nanos` is not strictly positive.
    pub const fn from_nanos(nanos: i64) -> Result<Self, InvalidResolution> {
        if nanos > 0 { Ok(Self(nanos)) } else { Err(InvalidResolution(nanos)) }
    }

    /// Creates a resolution from a [`Duration`].
    ///
    /// # Errors
    /// Returns [`InvalidResolution`] if the duration is zero.
    pub fn from_duration(d: Duration) -> Result<Self, InvalidResolution> {
        Self::from_nanos(duration_nanos(d))
    }

    /// Creates a resolution of whole seconds.
    ///
    /// # Panics
    /// Panics if `secs` is zero; intended for constants and tests.
    #[must_use]
    pub const fn from_secs(secs: u32) -> Self {
        assert!(secs > 0, "resolution must be positive");
        Self(secs as i64 * NANOS_PER_SEC)
    }

    /// Width of the window in nanoseconds.
    #[must_use]
    pub const fn as_nanos(self) -> i64 {
        self.0
    }

    /// Width of the window in seconds.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / 1e9
    }

    /// The window that contains `ts` (floor division, correct for negative times).
    #[must_use]
    pub const fn window_of(self, ts: Timestamp) -> WindowIdx {
        WindowIdx(ts.0.div_euclid(self.0))
    }

    /// Inclusive start of a window.
    #[must_use]
    pub const fn start_of(self, w: WindowIdx) -> Timestamp {
        Timestamp(w.0.saturating_mul(self.0))
    }

    /// Exclusive end of a window.
    #[must_use]
    pub const fn end_of(self, w: WindowIdx) -> Timestamp {
        Timestamp(w.0.saturating_add(1).saturating_mul(self.0))
    }

    /// Number of whole windows needed to cover `d` (at least one).
    #[must_use]
    pub fn windows_in(self, d: Duration) -> usize {
        let nanos = duration_nanos(d).unsigned_abs();
        let n = nanos.div_ceil(self.0.unsigned_abs()).max(1);
        usize::try_from(n).unwrap_or(usize::MAX)
    }
}

impl TryFrom<i64> for Resolution {
    type Error = InvalidResolution;
    fn try_from(v: i64) -> Result<Self, Self::Error> {
        Self::from_nanos(v)
    }
}

impl From<Resolution> for i64 {
    fn from(r: Resolution) -> Self {
        r.0
    }
}

/// Index of an aggregation window: window `w` covers `[w * res, (w + 1) * res)`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowIdx(pub i64);

impl WindowIdx {
    /// The next window.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }

    /// Offsets the window index by `n` windows.
    #[must_use]
    pub const fn offset(self, n: i64) -> Self {
        Self(self.0.saturating_add(n))
    }
}

/// Error returned by [`parse_duration`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid duration `{0}`: expected a number followed by ns, us, ms, s, m, h or d (e.g. `10s`, `1m30s`)")]
pub struct InvalidDuration(pub String);

/// Parses a human-readable duration such as `250ms`, `10s`, `1m30s` or `2h`.
///
/// A bare number is interpreted as seconds.
///
/// # Errors
/// Returns [`InvalidDuration`] for anything else, including overflow.
pub fn parse_duration(input: &str) -> Result<Duration, InvalidDuration> {
    let err = || InvalidDuration(input.to_owned());
    let s = input.trim();
    if s.is_empty() {
        return Err(err());
    }
    if let Ok(secs) = s.parse::<u64>() {
        return Ok(Duration::from_secs(secs));
    }
    let mut total: u128 = 0;
    let mut rest = s;
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !c.is_ascii_digit()).ok_or_else(err)?;
        if digits == 0 {
            return Err(err());
        }
        let value: u128 = rest[..digits].parse().map_err(|_| err())?;
        rest = &rest[digits..];
        let unit_len = rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len());
        let factor: u128 = match &rest[..unit_len] {
            "ns" => 1,
            "us" | "µs" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            "d" => 86_400_000_000_000,
            _ => return Err(err()),
        };
        total = total.checked_add(value.checked_mul(factor).ok_or_else(err)?).ok_or_else(err)?;
        rest = &rest[unit_len..];
    }
    let secs = u64::try_from(total / 1_000_000_000).map_err(|_| err())?;
    #[allow(clippy::cast_possible_truncation)]
    Ok(Duration::new(secs, (total % 1_000_000_000) as u32))
}

/// Formats a duration in the compact form accepted by [`parse_duration`].
#[must_use]
pub fn format_duration(d: Duration) -> String {
    let nanos = d.as_nanos();
    for (unit, factor) in
        [("h", 3_600_000_000_000u128), ("m", 60_000_000_000), ("s", 1_000_000_000), ("ms", 1_000_000), ("us", 1_000)]
    {
        if nanos >= factor && nanos.is_multiple_of(factor) {
            return format!("{}{unit}", nanos / factor);
        }
    }
    format!("{nanos}ns")
}

/// Serde adapter for [`Duration`] fields written as human-readable strings.
pub mod serde_duration {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialises as a compact string such as `10s`.
    ///
    /// # Errors
    /// Never fails for string-capable serialisers.
    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::format_duration(*d))
    }

    /// Accepts a string (`"10s"`) or a number of seconds.
    ///
    /// # Errors
    /// Fails on malformed durations.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Text(String),
            Secs(u64),
            Float(f64),
        }
        match Repr::deserialize(d)? {
            Repr::Text(t) => super::parse_duration(&t).map_err(serde::de::Error::custom),
            Repr::Secs(n) => Ok(Duration::from_secs(n)),
            Repr::Float(f) if f.is_finite() && f >= 0.0 => Ok(Duration::from_secs_f64(f)),
            Repr::Float(f) => Err(serde::de::Error::custom(format!("invalid duration {f}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_human_durations() {
        assert_eq!(parse_duration("10s"), Ok(Duration::from_secs(10)));
        assert_eq!(parse_duration("250ms"), Ok(Duration::from_millis(250)));
        assert_eq!(parse_duration("1m30s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_duration("2h"), Ok(Duration::from_secs(7200)));
        assert_eq!(parse_duration("15"), Ok(Duration::from_secs(15)));
        assert_eq!(parse_duration(" 5us "), Ok(Duration::from_micros(5)));
        for bad in ["", "s", "10x", "-5s", "1.5s", "10s5", "99999999999999999999999h"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn formats_round_trip() {
        for d in
            [Duration::from_secs(90), Duration::from_millis(250), Duration::from_secs(7200), Duration::from_nanos(7)]
        {
            assert_eq!(parse_duration(&format_duration(d)), Ok(d));
        }
        assert_eq!(format_duration(Duration::from_secs(120)), "2m");
    }

    #[test]
    fn window_assignment_handles_negative_time() {
        let r = Resolution::from_secs(10);
        assert_eq!(r.window_of(Timestamp::from_secs(0)), WindowIdx(0));
        assert_eq!(r.window_of(Timestamp::from_secs(9)), WindowIdx(0));
        assert_eq!(r.window_of(Timestamp::from_secs(10)), WindowIdx(1));
        assert_eq!(r.window_of(Timestamp(-1)), WindowIdx(-1));
        assert_eq!(r.start_of(WindowIdx(-1)), Timestamp::from_secs(-10));
        assert_eq!(r.end_of(WindowIdx(2)), Timestamp::from_secs(30));
    }

    #[test]
    fn windows_in_rounds_up() {
        let r = Resolution::from_secs(10);
        assert_eq!(r.windows_in(Duration::from_secs(1)), 1);
        assert_eq!(r.windows_in(Duration::from_secs(10)), 1);
        assert_eq!(r.windows_in(Duration::from_secs(11)), 2);
        assert_eq!(r.windows_in(Duration::ZERO), 1);
    }

    #[test]
    fn resolution_rejects_non_positive() {
        assert_eq!(Resolution::from_nanos(0), Err(InvalidResolution(0)));
        assert!(Resolution::from_duration(Duration::from_millis(1)).is_ok());
        let parsed: Result<Resolution, _> = serde_json::from_str("-5");
        assert!(parsed.is_err());
    }

    #[test]
    fn display_is_seconds_with_nanos() {
        assert_eq!(Timestamp(1_500_000_000).to_string(), "1.500000000s");
        assert_eq!(Timestamp(-1).to_string(), "-0.000000001s");
    }
}
