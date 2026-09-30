//! OpenTelemetry semantic-convention attribute keys the engine reads.
//!
//! Older and newer spellings are both accepted where the conventions
//! changed (for example `http.status_code` → `http.response.status_code`).

/// Logical service name.
pub const SERVICE_NAME: &str = "service.name";
/// Service namespace.
pub const SERVICE_NAMESPACE: &str = "service.namespace";
/// Kubernetes deployment.
pub const K8S_DEPLOYMENT_NAME: &str = "k8s.deployment.name";
/// Kubernetes stateful set.
pub const K8S_STATEFULSET_NAME: &str = "k8s.statefulset.name";
/// Kubernetes daemon set.
pub const K8S_DAEMONSET_NAME: &str = "k8s.daemonset.name";
/// Host name.
pub const HOST_NAME: &str = "host.name";

/// Logical name of the remote service of a client span.
pub const PEER_SERVICE: &str = "peer.service";
/// Database system (current spelling).
pub const DB_SYSTEM_NAME: &str = "db.system.name";
/// Database system (older spelling).
pub const DB_SYSTEM: &str = "db.system";
/// Messaging system.
pub const MESSAGING_SYSTEM: &str = "messaging.system";
/// Remote server address.
pub const SERVER_ADDRESS: &str = "server.address";
/// Remote peer name (older spelling).
pub const NET_PEER_NAME: &str = "net.peer.name";
/// HTTP response status code (current spelling).
pub const HTTP_RESPONSE_STATUS_CODE: &str = "http.response.status_code";
/// HTTP response status code (older spelling).
pub const HTTP_STATUS_CODE: &str = "http.status_code";
/// gRPC status code.
pub const RPC_GRPC_STATUS_CODE: &str = "rpc.grpc.status_code";

/// Peer attributes in priority order: the most specific name wins.
pub const PEER_KEYS: [&str; 6] =
    [PEER_SERVICE, DB_SYSTEM_NAME, DB_SYSTEM, MESSAGING_SYSTEM, SERVER_ADDRESS, NET_PEER_NAME];
