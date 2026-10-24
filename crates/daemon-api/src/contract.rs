//! HTTP schemas shared by server, clients and generated OpenAPI/SDK.
pub use okytes_core::desktop::{Action, AddInput, Job, Segment, Settings, Snapshot, Submit};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Monotonic capability hierarchy; control cannot manage token credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Jobs, snapshots, settings, contract and events.
    ReadOnly,
    /// Read plus job and queue controls.
    Control,
    /// Control plus settings and token management.
    Admin,
}

/// Stable sanitized error response. Never contains submitted URLs or tokens.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiError {
    /// Machine-readable error code.
    pub error: String,
    /// Stable broad category for SDK branching.
    pub category: ErrorCategory,
    /// Whether retrying later may succeed without changing the request.
    pub retryable: bool,
    /// Sanitized user-facing summary with no submitted URL or credential.
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Validation,
    Authentication,
    Authorization,
    Conflict,
    NotFound,
    RateLimit,
    Timeout,
    Unavailable,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Capabilities {
    pub api_version: String,
    pub sdk_compatibility: String,
    pub protocols: Vec<ProtocolCapability>,
    pub event_delivery: String,
    pub max_page_size: u32,
    pub max_batch_size: u32,
    pub idempotency_retention_seconds: u32,
    pub destination_semantics: String,
    pub credential_policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProtocolCapability {
    pub protocol: String,
    pub operations: Vec<String>,
    pub resume: bool,
    pub integrity: bool,
    pub approval_required: bool,
    pub credential_kind: Option<String>,
}

/// Explicit native approval for one short-lived local browser session.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct QuickPairing {
    /// Credential-free local bookmark URL.
    pub url: String,
    /// One-use approval code displayed only by the trusted native client.
    pub code: String,
    /// Positive code lifetime in seconds.
    pub expires_in: u32,
}

/// Administration request. The maximum permitted lifetime is 90 days.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct IssueToken {
    /// Capability scope.
    pub scope: Scope,
    /// Positive expiry in seconds, at most 7,776,000.
    pub lifetime_secs: u32,
}

/// Public token metadata; contains no token or stored digest.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TokenInfo {
    /// Revocation identifier.
    pub id: String,
    /// Effective capability scope.
    pub scope: Scope,
    /// Unix expiry timestamp.
    pub expires_at: i64,
    /// Whether this credential has been revoked.
    pub revoked: bool,
}

/// Returned only once over the authenticated admin connection.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct IssuedToken {
    /// Public metadata.
    pub info: TokenInfo,
    /// Bearer credential; clients must treat as a secret.
    pub token: String,
}

/// Bounded stream notifications. Clients must replace snapshots on Resync.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", content = "value")]
pub enum Update {
    /// Engine has changed; fetch an authoritative snapshot.
    Changed(okytes_core::service::EngineEvent),
    /// Initial subscription or dropped updates; discard stale local state.
    Resync,
}

/// Client endpoint selection; secrets are never included in Debug output.
pub struct Connection {
    /// Explicit HTTP loopback or verified HTTPS URL.
    pub url: String,
    /// Bearer token, read from a protected file by native apps.
    pub token: String,
}

/// Public half of this installation's persistent peer identity.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PeerIdentity {
    /// SHA-256 fingerprint shown on the trusted native surface.
    pub fingerprint: String,
    /// Base64 DER certificate exchanged out of band during pairing.
    pub certificate: String,
}

/// Fingerprint-confirmed trust update. The certificate is public material;
/// first approval uses no expected revision and replacement requires one.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PeerApproval {
    pub id: uuid::Uuid,
    pub certificate: String,
    pub confirmed_fingerprint: String,
    pub expected_revision: Option<u64>,
}

/// Public trust metadata returned to native management clients.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PeerTrust {
    pub id: uuid::Uuid,
    pub fingerprint: String,
    pub revision: u64,
    pub revoked: bool,
}

/// Compare-and-swap lifecycle decision for an admitted peer request.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PeerDecision {
    pub expected_revision: u64,
    pub action: PeerAction,
    /// Receiver-selected directory, used only when accepting inbound data.
    #[serde(default)]
    pub destination: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PeerAction {
    Accept,
    Reject,
    Pause,
    Retry,
    Cancel,
}

/// Immutable peer offer and its current durable lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PeerRequest {
    pub id: uuid::Uuid,
    pub peer_fingerprint: String,
    pub direction: okytes_core::peer::Direction,
    pub manifest: okytes_core::peer::Manifest,
    pub state: okytes_core::peer::State,
    pub revision: u64,
    pub bytes: u64,
    pub total: u64,
    pub files_completed: u32,
    pub failure: Option<String>,
    /// Local receiver destination; never populated for outbound requests.
    pub destination: Option<String>,
}

/// Manual-address outbound transfer. Source paths are consumed only by the
/// local native daemon and are never placed in the wire manifest.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PeerSend {
    pub peer_id: uuid::Uuid,
    pub address: String,
    pub paths: Vec<String>,
}

/// Unauthenticated DNS-SD address hint. `trusted` only means its advertised
/// fingerprint currently matches a local pin; TLS still proves the identity.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PeerDevice {
    pub peer_id: Option<uuid::Uuid>,
    pub fullname: String,
    pub name: String,
    pub os: String,
    pub protocol: u16,
    pub fingerprint: String,
    pub addresses: Vec<String>,
    pub port: u16,
    pub trusted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PeerDiscoverySetting {
    pub enabled: bool,
}
