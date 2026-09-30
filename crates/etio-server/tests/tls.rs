//! TLS and mutual TLS on every listener, with certificates generated per test.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::Path;

use common::{T0, config};
use etio_core::rng::Rng;
use etio_otlp::proto::collector::trace::v1::trace_service_client::TraceServiceClient;
use etio_server::actor::Clock;
use etio_server::config::TlsConfig;
use etio_server::serve::Running;
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity};

struct Pki {
    ca: String,
    server: (String, String),
    client: (String, String),
}

fn pki() -> Pki {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();
    let leaf = |names: Vec<String>| {
        let key = KeyPair::generate().unwrap();
        let cert = CertificateParams::new(names).unwrap().signed_by(&key, &ca).unwrap();
        (cert.pem(), key.serialize_pem())
    };
    Pki {
        ca: ca.pem(),
        server: leaf(vec!["127.0.0.1".into(), "localhost".into()]),
        client: leaf(vec!["edge-0".into()]),
    }
}

fn write(dir: &Path, pki: &Pki, mtls: bool) -> TlsConfig {
    let p = |name: &str, body: &str| {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    };
    TlsConfig {
        cert_file: p("server.pem", &pki.server.0),
        key_file: p("server.key", &pki.server.1),
        client_ca_file: mtls.then(|| p("ca.pem", &pki.ca)),
    }
}

fn http_client(pki: &Pki, identity: bool) -> reqwest::Client {
    let mut b =
        reqwest::Client::builder().add_root_certificate(reqwest::Certificate::from_pem(pki.ca.as_bytes()).unwrap());
    if identity {
        let pem = format!("{}{}", pki.client.0, pki.client.1);
        b = b.identity(reqwest::Identity::from_pem(pem.as_bytes()).unwrap());
    }
    b.build().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_on_the_api_and_otlp() {
    let dir = tempfile::tempdir().unwrap();
    let pki = pki();
    let mut cfg = config(None);
    cfg.tls = Some(write(dir.path(), &pki, false));
    let server = Running::start(cfg, Clock::Manual).await.unwrap();
    let api = server.bound.api.unwrap();

    let status = http_client(&pki, false).get(format!("https://{api}/api/v1/status")).send().await.unwrap();
    assert_eq!(status.status(), 200);
    // Plain HTTP is not served on a TLS listener.
    let plain = reqwest::get(format!("http://{api}/api/v1/status")).await;
    assert!(plain.map_or(true, |r| !r.status().is_success()), "plain HTTP answered");
    // A client that does not trust the CA is refused.
    let untrusted = reqwest::Client::new();
    assert!(untrusted.get(format!("https://{api}/api/v1/status")).send().await.is_err());

    // OTLP/gRPC over TLS.
    let tls = ClientTlsConfig::new().ca_certificate(Certificate::from_pem(&pki.ca)).domain_name("localhost");
    let channel = Endpoint::from_shared(format!("https://{}", server.bound.otlp_grpc.unwrap()))
        .unwrap()
        .tls_config(tls)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = TraceServiceClient::new(channel);
    let mut rng = Rng::seed_from_u64(3);
    client.export(common::traces(&mut rng, T0, 0)).await.unwrap();
    server.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutual_tls_requires_a_client_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let pki = pki();
    let mut cfg = config(None);
    cfg.tls = Some(write(dir.path(), &pki, true));
    let server = Running::start(cfg, Clock::Manual).await.unwrap();
    let url = format!("https://{}/api/v1/status", server.bound.api.unwrap());

    assert!(http_client(&pki, false).get(&url).send().await.is_err(), "no client certificate");
    let ok = http_client(&pki, true).get(&url).send().await.unwrap();
    assert_eq!(ok.status(), 200);

    let grpc = format!("https://{}", server.bound.otlp_grpc.unwrap());
    let base = ClientTlsConfig::new().ca_certificate(Certificate::from_pem(&pki.ca)).domain_name("localhost");
    let anonymous = Endpoint::from_shared(grpc.clone()).unwrap().tls_config(base.clone()).unwrap().connect().await;
    let refused = match anonymous {
        Err(_) => true,
        Ok(ch) => {
            let mut rng = Rng::seed_from_u64(4);
            TraceServiceClient::new(ch).export(common::traces(&mut rng, T0, 0)).await.is_err()
        }
    };
    assert!(refused, "gRPC without a client certificate");
    let with_identity = base.identity(Identity::from_pem(&pki.client.0, &pki.client.1));
    let ch = Endpoint::from_shared(grpc).unwrap().tls_config(with_identity).unwrap().connect().await.unwrap();
    let mut rng = Rng::seed_from_u64(5);
    TraceServiceClient::new(ch).export(common::traces(&mut rng, T0, 0)).await.unwrap();
    server.stop().await.unwrap();
}
