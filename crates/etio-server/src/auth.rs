//! Bearer-token authentication.
//!
//! Two independent tokens: one to send telemetry, one to read the API.
//! Tokens are compared in constant time so that response timing does not
//! leak how much of a guess was right.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use subtle::ConstantTimeEq;

use crate::config::{AuthConfig, Secret};

/// Loaded tokens.
#[derive(Debug, Default)]
pub struct Auth {
    ingest: Option<Secret>,
    read: Option<Secret>,
}

fn matches(expected: Option<&Secret>, authorization: Option<&str>) -> bool {
    let Some(expected) = expected else { return true };
    let Some(given) = authorization.and_then(|h| h.strip_prefix("Bearer ")) else { return false };
    given.as_bytes().ct_eq(expected.expose().as_bytes()).into()
}

impl Auth {
    /// Reads the token files named by the configuration.
    ///
    /// # Errors
    /// Fails if a configured file cannot be read or is empty.
    pub fn from_config(cfg: &AuthConfig) -> anyhow::Result<Self> {
        Ok(Self {
            ingest: cfg.ingest_token_file.as_deref().map(Secret::from_file).transpose()?,
            read: cfg.read_token_file.as_deref().map(Secret::from_file).transpose()?,
        })
    }

    /// Tokens given directly (tests).
    #[must_use]
    pub fn with_tokens(ingest: Option<Secret>, read: Option<Secret>) -> Self {
        Self { ingest, read }
    }

    /// Whether an `Authorization` header value grants ingestion.
    #[must_use]
    pub fn may_ingest(&self, authorization: Option<&str>) -> bool {
        matches(self.ingest.as_ref(), authorization)
    }

    /// Whether an `Authorization` header value grants reading.
    #[must_use]
    pub fn may_read(&self, authorization: Option<&str>) -> bool {
        matches(self.read.as_ref(), authorization)
    }

    /// Whether ingestion is protected.
    #[must_use]
    pub const fn ingest_protected(&self) -> bool {
        self.ingest.is_some()
    }
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer")], "missing or invalid bearer token")
        .into_response()
}

/// Axum middleware guarding API routes.
pub async fn require_read(State(auth): State<Arc<Auth>>, req: Request, next: Next) -> Response {
    let h = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    if auth.may_read(h) { next.run(req).await } else { unauthorized() }
}

/// Axum middleware guarding OTLP/HTTP routes.
pub async fn require_ingest(State(auth): State<Arc<Auth>>, req: Request, next: Next) -> Response {
    let h = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    if auth.may_ingest(h) { next.run(req).await } else { unauthorized() }
}

/// A tonic interceptor guarding the OTLP gRPC services.
#[derive(Clone)]
pub struct GrpcAuth(pub Arc<Auth>);

impl tonic::service::Interceptor for GrpcAuth {
    fn call(&mut self, req: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
        let h = req.metadata().get("authorization").and_then(|v| v.to_str().ok());
        if self.0.may_ingest(h) {
            Ok(req)
        } else {
            Err(tonic::Status::unauthenticated("missing or invalid bearer token"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(s: &str) -> Secret {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t");
        std::fs::write(&p, s).unwrap();
        Secret::from_file(&p).unwrap()
    }

    #[test]
    fn open_when_unconfigured_and_strict_when_configured() {
        let open = Auth::default();
        assert!(open.may_ingest(None) && open.may_read(None));
        let auth = Auth::with_tokens(Some(secret("in")), Some(secret("rd")));
        assert!(auth.may_ingest(Some("Bearer in")));
        assert!(!auth.may_ingest(Some("Bearer rd")));
        assert!(!auth.may_ingest(Some("in")));
        assert!(!auth.may_ingest(None));
        assert!(auth.may_read(Some("Bearer rd")));
        assert!(!auth.may_read(Some("Bearer r")));
    }
}
