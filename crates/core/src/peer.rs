//! Portable peer admission contract. Nothing in this module opens a listener,
//! establishes trust, touches the filesystem, or authorizes a transfer.
//! Manifests are immutable commitments; received paths are never silently fixed.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use unicode_normalization::{is_nfc, UnicodeNormalization};
use utoipa::ToSchema;
use uuid::Uuid;

/// Current internal control-protocol version.
pub const VERSION: u16 = 1;
/// Hard bound on JSON control payloads, excluding the four-byte length prefix.
pub const MAX_FRAME: usize = 1024 * 1024;
/// Hard bound on entries, including explicitly represented empty directories.
pub const MAX_ENTRIES: usize = 1024;
/// Maximum aggregate offered content (one TiB), independent of frame size.
pub const MAX_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
/// Maximum binary payload carried by one peer data frame.
pub const MAX_CHUNK: usize = 256 * 1024;

/// Deliberately generic errors do not echo untrusted paths or wire content.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid or ambiguous relative path.
    #[error("unsafe or ambiguous peer path")]
    Path,
    /// Invalid file, digest, ordering or directory hierarchy.
    #[error("invalid peer manifest")]
    Manifest,
    /// Resource policy exceeded.
    #[error("peer request exceeds resource limits")]
    Limit,
    /// Invalid control envelope, length or protocol version.
    #[error("invalid or unsupported peer control frame")]
    Frame,
    /// Invalid lifecycle transition.
    #[error("invalid peer transfer state transition")]
    State,
}

/// Lowercase SHA-256 commitment, not a credential or evidence of device trust.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(try_from = "String", into = "String")]
pub struct Hash(String);

impl Hash {
    /// Hash bounded metadata or a caller-supplied byte slice.
    pub fn digest(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }
    /// Canonical lowercase hexadecimal representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for Hash {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Manifest);
        }
        Ok(Self(value))
    }
}
impl From<Hash> for String {
    fn from(value: Hash) -> Self {
        value.0
    }
}

/// Only regular files and directories can appear on the wire. Links, device
/// objects, permissions and sender absolute paths have no representation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Entry {
    /// Immutable file length and SHA-256 expected before publication.
    File {
        /// Exact NFC, slash-separated relative path.
        path: String,
        /// Expected content length, checked against aggregate policy.
        size: u64,
        /// Expected content digest; never a substitute for checking file bytes.
        sha256: Hash,
    },
    /// Explicit directory, including empty ones.
    Directory {
        /// Exact NFC relative path; parents must also be explicit entries.
        path: String,
    },
}
impl Entry {
    /// Sender-relative path, using slash separators on every platform.
    pub fn path(&self) -> &str {
        match self {
            Self::File { path, .. } | Self::Directory { path } => path,
        }
    }
}

/// Immutable ordered transfer metadata. Call validate after deserialization;
/// decode_frame and persistent admission both enforce that boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Entries sorted by exact UTF-8 path, parents before children.
    pub entries: Vec<Entry>,
}

/// Canonical path comparison for admission, not filesystem containment.
/// NFC is required on the wire. NFKC plus upper/lower casing is deliberately
/// conservative for cross-platform collisions (e.g. sharp-s and final sigma).
pub fn path_key(path: &str) -> Result<String, Error> {
    if path.is_empty() || path.len() > 1024 || !is_nfc(path) || path.split('/').count() > 32 {
        return Err(Error::Path);
    }
    for part in path.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.len() > 255
            || part.ends_with([' ', '.'])
            || part.chars().any(|c| {
                c.is_control()
                    || "\\:<>\"|?*".contains(c)
                    || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            })
        {
            return Err(Error::Path);
        }
        let stem = part
            .split('.')
            .next()
            .ok_or(Error::Path)?
            .trim_end_matches(' ')
            .to_uppercase();
        if matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
        ) || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        }) {
            return Err(Error::Path);
        }
    }
    Ok(path
        .nfkc()
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .nfkc()
        .collect())
}

impl Manifest {
    /// Sort locally constructed entries and reject ambiguous metadata.
    pub fn new(mut entries: Vec<Entry>) -> Result<Self, Error> {
        if entries.len() > MAX_ENTRIES {
            return Err(Error::Limit);
        }
        entries.sort_by(|a, b| a.path().cmp(b.path()));
        let result = Self { entries };
        result.validate()?;
        Ok(result)
    }
    /// Validate limits, canonical order, paths, collisions and explicit parents;
    /// return the aggregate byte count using checked 64-bit arithmetic.
    pub fn validate(&self) -> Result<u64, Error> {
        if self.entries.is_empty() || self.entries.len() > MAX_ENTRIES {
            return Err(Error::Limit);
        }
        let mut seen = BTreeMap::new();
        let mut previous: Option<&str> = None;
        let mut total = 0u64;
        for entry in &self.entries {
            let path = entry.path();
            if previous.is_some_and(|old| old >= path) {
                return Err(Error::Manifest);
            }
            previous = Some(path);
            let key = path_key(path)?;
            if seen.contains_key(&key) {
                return Err(Error::Path);
            }
            if let Some((parent, _)) = path.rsplit_once('/') {
                // Exact spelling matters too: no silently remapped directory.
                if !matches!(seen.get(&path_key(parent)?), Some((old, true)) if *old == parent) {
                    return Err(Error::Manifest);
                }
            }
            let directory = matches!(entry, Entry::Directory { .. });
            seen.insert(key, (path, directory));
            if let Entry::File { size, .. } = entry {
                total = total
                    .checked_add(*size)
                    .filter(|n| *n <= MAX_BYTES)
                    .ok_or(Error::Limit)?;
            }
        }
        let bytes = serde_json::to_vec(self).map_err(|_| Error::Manifest)?;
        if bytes.len() > MAX_FRAME - 512 {
            return Err(Error::Limit);
        }
        Ok(total)
    }
    /// Domain-separated commitment over canonical metadata, including every
    /// relative path, object type, length and file hash.
    pub fn commitment(&self) -> Result<Hash, Error> {
        self.validate()?;
        let mut hash = Sha256::new();
        hash.update(b"okytes-peer-manifest-v1\0");
        hash.update(serde_json::to_vec(self).map_err(|_| Error::Manifest)?);
        Hash::try_from(format!("{:x}", hash.finalize()))
    }
}

/// Peer transfer direction, independent from HTTP download semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// The local user must decide whether to receive this offer.
    Inbound,
    /// The local sender is preparing or sending an offer.
    Outbound,
}

/// Peer-specific lifecycle. Approval is distinct from device pairing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Local sender prepared an offer that has not yet been submitted.
    Created,
    /// Waiting for an explicit receiver decision; not runnable.
    AwaitingApproval,
    /// Receiver approved; authenticated execution can be negotiated.
    Negotiating,
    /// An approved executor is transferring payload.
    Transferring,
    /// Payload is complete and awaiting integrity/publication checks.
    Verifying,
    /// Integrity checked and output published; terminal.
    Completed,
    /// Receiver declined the offer; terminal.
    Rejected,
    /// Approved transfer deliberately stopped by its user.
    Paused,
    /// Approved execution was interrupted by process/network loss.
    Interrupted,
    /// Explicit retry requested for a previously approved transfer.
    Retrying,
    /// Approved execution failed, preserving its diagnostic history.
    Failed,
    /// Transfer was withdrawn; terminal.
    Cancelled,
}
impl State {
    /// Legal, idempotent state progression. Completed/rejected/cancelled records
    /// cannot be reopened by replaying a late message. Payload executors must
    /// independently establish durable bytes and integrity before completion.
    pub fn transition(self, next: Self) -> Result<Self, Error> {
        use State::*;
        if self == next {
            return Ok(self);
        }
        let allowed = match self {
            Created => matches!(next, AwaitingApproval | Cancelled),
            AwaitingApproval => matches!(next, Negotiating | Rejected | Cancelled | Failed),
            Negotiating => matches!(
                next,
                Transferring | Paused | Interrupted | Cancelled | Failed
            ),
            Transferring => matches!(next, Verifying | Paused | Interrupted | Cancelled | Failed),
            Verifying => matches!(next, Completed | Failed | Interrupted),
            Paused | Interrupted | Failed => matches!(next, Retrying | Cancelled),
            Retrying => matches!(
                next,
                Negotiating | Paused | Interrupted | Cancelled | Failed
            ),
            Completed | Rejected | Cancelled => false,
        };
        if allowed {
            Ok(next)
        } else {
            Err(Error::State)
        }
    }
}

/// Strict versioned control envelope. Only an authenticated adapter may act on
/// these messages; successful parsing never authorizes an operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    /// Exact internal protocol version, without implicit downgrade.
    pub version: u16,
    /// Bounded control metadata, independent of transport credentials.
    pub message: Message,
}

/// Control metadata only; bulk payload framing belongs to the transport adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    /// An immutable offer. It is not permission to read or write files.
    Offer {
        /// Non-nil identifier bound to one immutable offer.
        request_id: Uuid,
        /// Validated metadata, without local absolute paths.
        manifest: Manifest,
    },
    /// A lifecycle observation; not an authorization from an untrusted sender.
    Status {
        /// Previously admitted transfer identifier.
        request_id: Uuid,
        /// Reported lifecycle; does not grant local approval.
        state: State,
        /// Monotonic revision, bounded by a signed 64-bit integer.
        revision: u64,
    },
}
impl Envelope {
    fn validate(&self) -> Result<(), Error> {
        if self.version != VERSION {
            return Err(Error::Frame);
        }
        let id = match &self.message {
            Message::Offer {
                request_id,
                manifest,
            } => {
                manifest.validate()?;
                request_id
            }
            Message::Status {
                request_id,
                revision,
                ..
            } => {
                if *revision > i64::MAX as u64 {
                    return Err(Error::Limit);
                }
                request_id
            }
        };
        if id.is_nil() {
            return Err(Error::Frame);
        }
        Ok(())
    }
}

/// Validate a length header before a transport allocates the payload buffer.
pub fn frame_length(header: [u8; 4]) -> Result<usize, Error> {
    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(Error::Limit);
    }
    Ok(size)
}

/// Encode one validated envelope with a four-byte network-order byte length.
pub fn encode_frame(value: &Envelope) -> Result<Vec<u8>, Error> {
    value.validate()?;
    let payload = serde_json::to_vec(value).map_err(|_| Error::Frame)?;
    let size = u32::try_from(payload.len()).map_err(|_| Error::Limit)?;
    frame_length(size.to_be_bytes())?;
    let mut result = Vec::with_capacity(payload.len() + 4);
    result.extend_from_slice(&size.to_be_bytes());
    result.extend_from_slice(&payload);
    Ok(result)
}

/// Decode exactly one frame. Reject truncation, trailing frames, unknown fields,
/// unsupported versions and invalid manifests before admission.
pub fn decode_frame(bytes: &[u8]) -> Result<Envelope, Error> {
    let header = bytes.get(..4).ok_or(Error::Frame)?;
    let size = frame_length(header.try_into().map_err(|_| Error::Frame)?)?;
    if bytes.len() != size + 4 {
        return Err(Error::Frame);
    }
    let value: Envelope = serde_json::from_slice(&bytes[4..]).map_err(|_| Error::Frame)?;
    value.validate()?;
    Ok(value)
}
