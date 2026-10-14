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

/// One authenticated browser handoff, preserving the existing native schema.
pub type Capture = okytes_browser_bridge::protocol::Request;

/// Client endpoint selection; secrets are never included in Debug output.
pub struct Connection {
    /// Explicit HTTP loopback or verified HTTPS URL.
    pub url: String,
    /// Bearer token, read from a protected file by native apps.
    pub token: String,
}
