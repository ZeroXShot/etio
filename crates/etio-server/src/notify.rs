//! Webhook notifications.
//!
//! Each incident event is POSTed as JSON to every webhook subscribed to it.
//! Deliveries carry the incident id in `X-Etio-Incident` (derived from the
//! data, hence identical across replicas: receivers can de-duplicate) and,
//! when a secret is configured, an HMAC-SHA256 signature of the body in
//! `X-Etio-Signature: sha256=<hex>`. Failed deliveries are retried with
//! exponential backoff.

use std::sync::Arc;
use std::time::Duration;

use etio_engine::{Event, Incident};
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use sha2::Sha256;
use tokio::sync::broadcast::error::RecvError;

use crate::actor::EngineHandle;
use crate::config::{Secret, Webhook};
use crate::metrics::DeliveryLabels;

/// Number of attempts per delivery.
const ATTEMPTS: u32 = 4;

#[derive(Serialize)]
struct Payload<'a> {
    event: &'static str,
    incident: &'a Incident,
    /// Top candidates, for receivers that do not want to parse the analysis.
    summary: Vec<(String, f64)>,
}

/// Signs a body with HMAC-SHA256.
#[must_use]
pub fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|_| unreachable!("HMAC accepts any key length"));
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

struct Target {
    hook: Webhook,
    secret: Option<Secret>,
}

impl Target {
    fn wants(&self, event: &str) -> bool {
        self.hook.events.is_empty() || self.hook.events.iter().any(|e| e == event)
    }
}

/// Background task delivering incident events to webhooks.
///
/// # Errors
/// Fails at start-up if a secret file cannot be read.
pub fn spawn(handle: EngineHandle, hooks: Vec<Webhook>) -> anyhow::Result<()> {
    if hooks.is_empty() {
        return Ok(());
    }
    let targets: Vec<Arc<Target>> = hooks
        .into_iter()
        .map(|hook| {
            let secret = hook.secret_file.as_deref().map(Secret::from_file).transpose()?;
            Ok(Arc::new(Target { hook, secret }))
        })
        .collect::<anyhow::Result<_>>()?;
    let client = reqwest::Client::builder().user_agent(concat!("etio/", env!("CARGO_PKG_VERSION"))).build()?;
    let mut events = handle.subscribe();
    tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(e) => e,
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(missed = n, "webhook delivery fell behind");
                    continue;
                }
                Err(RecvError::Closed) => break,
            };
            let (name, incident) = match &*event {
                Event::IncidentOpened { incident } => ("opened", incident),
                Event::IncidentAnalyzed { incident } => ("analyzed", incident),
                Event::IncidentResolved { incident } => ("resolved", incident),
            };
            let summary = incident
                .rca
                .as_ref()
                .map(|r| r.ranking.iter().take(5).map(|x| (x.service.clone(), x.probability)).collect())
                .unwrap_or_default();
            let Ok(body) = serde_json::to_vec(&Payload { event: name, incident, summary }) else { continue };
            let body = Arc::new(body);
            for target in targets.iter().filter(|t| t.wants(name)) {
                let (client, target, body, handle) = (client.clone(), target.clone(), body.clone(), handle.clone());
                let id = incident.id.clone();
                tokio::spawn(async move { deliver(&client, &target, &body, name, &id, &handle).await });
            }
        }
    });
    Ok(())
}

async fn deliver(client: &reqwest::Client, target: &Target, body: &[u8], event: &str, id: &str, handle: &EngineHandle) {
    let mut backoff = Duration::from_millis(500);
    for attempt in 1..=ATTEMPTS {
        let mut req = client
            .post(&target.hook.url)
            .timeout(target.hook.timeout)
            .header("content-type", "application/json")
            .header("x-etio-event", event)
            .header("x-etio-incident", id)
            .body(body.to_vec());
        if let Some(secret) = &target.secret {
            req = req.header("x-etio-signature", sign(secret.expose(), body));
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                handle.metrics().deliveries.get_or_create(&DeliveryLabels { outcome: "ok" }).inc();
                return;
            }
            Ok(resp) if resp.status().is_client_error() && resp.status().as_u16() != 429 => {
                tracing::warn!(url = %target.hook.url, status = %resp.status(), "webhook rejected the delivery");
                break;
            }
            Ok(resp) => {
                tracing::debug!(url = %target.hook.url, status = %resp.status(), attempt, "webhook delivery failed");
            }
            Err(e) => tracing::debug!(url = %target.hook.url, error = %e, attempt, "webhook delivery failed"),
        }
        tokio::time::sleep(backoff).await;
        backoff *= 2;
    }
    handle.metrics().deliveries.get_or_create(&DeliveryLabels { outcome: "failed" }).inc();
}

#[cfg(test)]
mod tests {
    use super::sign;

    #[test]
    fn signatures_match_the_rfc_4231_vector() {
        // RFC 4231, test case 2.
        let s = sign("Jefe", b"what do ya want for nothing?");
        assert_eq!(s, "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }
}
