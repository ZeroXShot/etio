//! TLS and mutual TLS for the HTTP listeners.
//!
//! The gRPC listener uses tonic's built-in TLS; the HTTP listeners (OTLP/HTTP
//! and the API) are served here by a small accept loop over `tokio-rustls`
//! and `hyper-util`, so the same certificate, key and optional client CA
//! apply everywhere.

use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::config::TlsConfig;

fn certs(path: &std::path::Path) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let pem = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let certs = CertificateDer::pem_slice_iter(&pem).collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(!certs.is_empty(), "no certificate in {}", path.display());
    Ok(certs)
}

fn key(path: &std::path::Path) -> anyhow::Result<PrivateKeyDer<'static>> {
    let pem = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    PrivateKeyDer::from_pem_slice(&pem).with_context(|| format!("no private key in {}", path.display()))
}

/// Client TLS for edge-to-core streams, trusting the CA bundle at `ca_file`.
///
/// # Errors
/// Fails if the bundle cannot be read.
pub fn client_config(ca_file: &std::path::Path) -> anyhow::Result<tonic::transport::ClientTlsConfig> {
    let ca = std::fs::read(ca_file).with_context(|| format!("reading {}", ca_file.display()))?;
    Ok(tonic::transport::ClientTlsConfig::new().ca_certificate(tonic::transport::Certificate::from_pem(ca)))
}

/// Builds the rustls server configuration (HTTP/1.1 and HTTP/2 via ALPN).
///
/// # Errors
/// Fails if a file cannot be read or does not contain valid PEM.
pub fn server_config(cfg: &TlsConfig) -> anyhow::Result<Arc<rustls::ServerConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder =
        rustls::ServerConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions()?;
    let builder = match &cfg.client_ca_file {
        Some(ca) => {
            let mut roots = rustls::RootCertStore::empty();
            for c in certs(ca)? {
                roots.add(c)?;
            }
            let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider).build()?;
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    };
    let mut config = builder.with_single_cert(certs(&cfg.cert_file)?, key(&cfg.key_file)?)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// The tonic TLS configuration equivalent to [`server_config`].
///
/// # Errors
/// Fails if a file cannot be read.
pub fn grpc_config(cfg: &TlsConfig) -> anyhow::Result<tonic::transport::ServerTlsConfig> {
    let cert = std::fs::read(&cfg.cert_file).with_context(|| format!("reading {}", cfg.cert_file.display()))?;
    let key = std::fs::read(&cfg.key_file).with_context(|| format!("reading {}", cfg.key_file.display()))?;
    let mut tls = tonic::transport::ServerTlsConfig::new().identity(tonic::transport::Identity::from_pem(cert, key));
    if let Some(ca) = &cfg.client_ca_file {
        let ca = std::fs::read(ca).with_context(|| format!("reading {}", ca.display()))?;
        tls = tls.client_ca_root(tonic::transport::Certificate::from_pem(ca));
    }
    Ok(tls)
}

/// Serves `router` over TLS until `shutdown` resolves.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    config: Arc<rustls::ServerConfig>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let acceptor = TlsAcceptor::from(config);
    loop {
        let (tcp, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!(error = %e, "accept failed");
                    continue;
                }
            },
            _ = shutdown.changed() => break,
        };
        let (acceptor, router) = (acceptor.clone(), router.clone());
        tokio::spawn(async move {
            let stream = match acceptor.accept(tcp).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(%peer, error = %e, "TLS handshake failed");
                    return;
                }
            };
            let service = hyper_util::service::TowerToHyperService::new(router);
            if let Err(e) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                tracing::debug!(%peer, error = %e, "connection error");
            }
        });
    }
}
