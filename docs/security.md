# Security

This page is the threat model. To report a vulnerability, see
[SECURITY.md](../SECURITY.md).

## Assets

* **Telemetry** flowing through Etio: service names, endpoints, error
  messages in logs, and whatever attributes applications attach (which can
  include personal data if applications log it).
* **Analysis results**: incidents, rankings, operator feedback.
* **Availability**: Etio is part of incident response; it must keep working
  when the systems it watches misbehave.

## Trust boundaries

| boundary | who is on the other side | controls |
|---|---|---|
| OTLP receivers | applications, Collectors | ingest token, TLS/mTLS, size and decompression limits, backpressure |
| API and UI | operators, automation | read token, TLS/mTLS |
| edge ↔ core | other Etio nodes | cluster token, TLS (CA-verified), protocol version checks |
| webhooks | notification receivers | HMAC-SHA256 signatures, timeouts |
| state directory | the host | file permissions; contains telemetry aggregates |

## Threats and mitigations

**Unauthenticated access.** Without token files, endpoints are open, which is
only appropriate on a trusted network. Production deployments should set
`auth.ingest_token_file`, `auth.read_token_file` and
`cluster.token_file`. Tokens are compared in constant time. Secrets are only
ever read from files (Kubernetes/Docker secrets); the configuration names
files, never values, and `Secret` values are redacted from debug output.

**Eavesdropping and tampering.** `[tls]` enables TLS on every listener
(rustls; TLS 1.2+; no OpenSSL). `client_ca_file` turns on mutual TLS. Edges
verify cores against `cluster.ca_file`.

**Resource exhaustion.** Every input is bounded:

* request size (`limits.max_request_bytes`) and size after decompression
  (`limits.max_decompressed_bytes`), which defeats decompression bombs;
* a bounded ingest queue with protocol-level backpressure instead of
  unbounded buffering;
* bounded span buffers per trace and in total, bounded open windows (far
  future timestamps are rejected), bounded series, interned strings, log
  templates and counter streams. Every bound has a counter in `/metrics`;
* the API validates and caps parameters (limits, feedback sizes).

A client that is allowed to ingest can still create many series (high
cardinality service names) up to the configured limits; it cannot exhaust
memory.

**Malformed input.** The OTLP decoders never panic on malformed input
(fuzzed; `cargo fuzz` targets in `fuzz/`), and the workspace forbids
`unsafe` code. Invalid items are counted and rejected, not fatal.

**Clock abuse.** A client can send timestamps far in the future or past.
Past data beyond `lateness` is dropped and counted; far-future data is
rejected by the aggregator. With `clock = "event"` a future timestamp would
move the engine clock for everyone, which is why that mode is meant for
replays, not production.

**Webhook receivers.** Deliveries are signed (`X-Etio-Signature`) when a
secret is configured, so receivers can reject forged calls. Etio only calls
the configured URLs (no user-controlled destinations: no SSRF surface).

**Supply chain.** Dependencies are pinned in `Cargo.lock`,
`package-lock.json` and `uv.lock`, audited in CI (`cargo deny`, `cargo
audit`, `npm audit`), and the container image ships an SBOM. The image is
distroless, runs as a non-root user and has no shell.

**Data at rest.** The state directory holds window aggregates, incidents
and feedback, not raw telemetry, but aggregates still reveal service names
and error rates. Protect it with file permissions or volume encryption.

## Out of scope

* Multi-tenancy: one Etio instance serves one trust domain.
* Authorisation finer than "may ingest" / "may read".
* Protecting against a malicious host or an attacker with access to the
  state directory.
