//! The engine's compact span representation.
//!
//! OTLP spans carry dozens of fields and arbitrary attributes; root-cause
//! analysis needs about ten of them. Decoders extract exactly these into a
//! [`Span`], interning strings so that the record is small, `Copy`, and cheap
//! to hash on.

use etio_core::Sym;
use serde::{Deserialize, Serialize};

/// OpenTelemetry span kind.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    /// Not specified by the instrumentation.
    #[default]
    Unspecified,
    /// An internal operation.
    Internal,
    /// Handles a synchronous request.
    Server,
    /// Issues a synchronous request.
    Client,
    /// Sends a message.
    Producer,
    /// Receives a message.
    Consumer,
}

impl SpanKind {
    /// Maps the OTLP enum value.
    #[must_use]
    pub const fn from_otlp(v: i32) -> Self {
        match v {
            1 => Self::Internal,
            2 => Self::Server,
            3 => Self::Client,
            4 => Self::Producer,
            5 => Self::Consumer,
            _ => Self::Unspecified,
        }
    }

    /// Whether the span represents work done *for* a remote caller.
    #[must_use]
    pub const fn is_inbound(self) -> bool {
        matches!(self, Self::Server | Self::Consumer)
    }

    /// Whether the span represents a call *to* a remote dependency.
    #[must_use]
    pub const fn is_outbound(self) -> bool {
        matches!(self, Self::Client | Self::Producer)
    }
}

/// OpenTelemetry status code.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus {
    /// Not set.
    #[default]
    Unset,
    /// Explicitly successful.
    Ok,
    /// Failed.
    Error,
}

/// A span, reduced to what root-cause analysis needs.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    /// Trace identifier.
    pub trace_id: u128,
    /// Span identifier (never zero for a valid span).
    pub span_id: u64,
    /// Parent span identifier, zero for a root span.
    pub parent_id: u64,
    /// Service that emitted the span.
    pub service: Sym,
    /// Operation (span name).
    pub operation: Sym,
    /// Span kind.
    pub kind: SpanKind,
    /// Start, in nanoseconds since the epoch.
    pub start: i64,
    /// End, in nanoseconds since the epoch.
    pub end: i64,
    /// Status.
    pub status: SpanStatus,
    /// For outbound spans, the remote peer if it is named by the span
    /// (`peer.service`, `db.system`, `server.address`, ...). Lets the engine
    /// represent dependencies that emit no telemetry of their own.
    pub peer: Sym,
}

impl Span {
    /// Duration in nanoseconds, clamped at zero.
    #[must_use]
    pub const fn duration(&self) -> i64 {
        let d = self.end.saturating_sub(self.start);
        if d < 0 { 0 } else { d }
    }

    /// Whether the span failed.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.status == SpanStatus::Error
    }
}
