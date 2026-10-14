//! Version-one browser messages. Secrets have no Debug implementation and never
//! belong in application history, preferences, error strings or tracing fields.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

/// Maximum JSON frame, including credentials; checked before allocating a body.
pub const MAX_FRAME: usize = 128 * 1024;
/// Native host registration name.
pub const HOST_NAME: &str = "org.okytes.bridge";

/// Explicit one-download session handoff; values remain memory-only.
#[derive(Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "../../../extensions/shared/generated/")]
pub struct Capture {
    /// HTTP(S) source URL without userinfo.
    pub url: String,
    /// Optional safe basename, never a path.
    pub filename: Option<String>,
    /// Optional publisher hash, useful for automated integrity checks.
    pub sha256: Option<String>,
    /// Origin-scoped headers supplied for this download only.
    pub headers: Vec<(String, String)>,
}

/// Operations available to browser clients. No arbitrary queue/admin access.
#[derive(Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[serde(tag = "kind", content = "value", deny_unknown_fields)]
#[ts(export, export_to = "../../../extensions/shared/generated/")]
pub enum Operation {
    /// Check the running desktop connection.
    Ping,
    /// Enqueue one explicit transfer.
    Capture(Capture),
}

/// Idempotency ID survives reconnects; protocol version is checked by Rust.
#[derive(Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "../../../extensions/shared/generated/")]
pub struct Request {
    /// Must be one.
    pub version: u32,
    /// Canonical UUID generated for one operation.
    pub id: String,
    /// Scoped operation.
    pub operation: Operation,
}

/// Sanitized acknowledgement; contains neither URL nor credentials.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "../../../extensions/shared/generated/")]
pub struct Response {
    /// Protocol version.
    pub version: u32,
    /// Echoed request ID.
    pub id: String,
    /// Accepted by the desktop, not merely delivered to the host.
    pub ok: bool,
    /// Persisted job UUID for successful capture.
    pub job_id: Option<String>,
    /// Stable, sanitized error code.
    pub error: Option<String>,
}

impl Response {
    /// Construct a response without leaking input or internal diagnostics.
    pub fn reply(id: String, result: Result<Option<String>, String>) -> Self {
        match result {
            Ok(job_id) => Self {
                version: 1,
                id,
                ok: true,
                job_id,
                error: None,
            },
            Err(error) => Self {
                version: 1,
                id,
                ok: false,
                job_id: None,
                error: Some(error),
            },
        }
    }
}

impl Request {
    /// Reject unknown versions, malformed IDs, unsafe URLs and reserved headers.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1 || Uuid::parse_str(&self.id).is_err() || self.id.len() != 36 {
            return Err("invalid_request");
        }
        if let Operation::Capture(capture) = &self.operation {
            let url = url::Url::parse(&capture.url).map_err(|_| "invalid_url")?;
            if capture.url.len() > 8192
                || !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.host_str().is_none()
            {
                return Err("invalid_url");
            }
            if capture.filename.as_ref().is_some_and(|name| {
                name.is_empty()
                    || name.len() > 200
                    || name.contains(['/', '\\'])
                    || name.chars().any(char::is_control)
            }) {
                return Err("invalid_filename");
            }
            if capture.sha256.as_ref().is_some_and(|hash| {
                hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            }) {
                return Err("invalid_hash");
            }
            validate_headers(&capture.headers)?;
        }
        Ok(())
    }
}

/// Only explicit origin-scoped browser session headers are supported. Restrict
/// this allowlist deliberately rather than forwarding arbitrary browser headers.
pub fn validate_headers(headers: &[(String, String)]) -> Result<(), &'static str> {
    if headers.len() > 16
        || headers
            .iter()
            .map(|(n, v)| n.len() + v.len())
            .sum::<usize>()
            > 32768
    {
        return Err("invalid_headers");
    }
    let mut names = std::collections::HashSet::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        if !matches!(
            name.as_str(),
            "cookie" | "authorization" | "referer" | "user-agent" | "accept"
        ) || !names.insert(name)
            || value.len() > 16384
            || !value.is_ascii()
            || value.bytes().any(|b| b < 32 || b == 127)
        {
            return Err("invalid_headers");
        }
    }
    Ok(())
}
