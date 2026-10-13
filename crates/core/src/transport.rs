//! Transport trait abstraction.
//!
//! Defines the interface that protocol-specific transports (HTTP, FTP, etc.)
//! must implement. This allows the engine to be transport-agnostic while
//! the HTTP transport in `transport-http` provides the first implementation.

use std::pin::Pin;

use bytes::Bytes;
use futures::Stream;
use url::Url;

use crate::error::TransportError;
use crate::types::RemoteIdentity;

/// Result of probing a remote resource.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    /// Final URL after redirects.
    pub final_url: Url,
    /// Total size in bytes, if the server reports it.
    pub content_length: Option<u64>,
    /// Whether the server supports byte-range requests.
    pub accepts_ranges: bool,
    /// Remote identity validators for resume verification.
    pub remote_identity: RemoteIdentity,
    /// Suggested filename from Content-Disposition or URL.
    pub suggested_filename: Option<String>,
    /// Content type (MIME) of the resource.
    pub content_type: Option<String>,
}

/// A streaming body of bytes from a transport.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, TransportError>> + Send>>;

/// Result of starting a ranged or full download.
pub struct FetchResult {
    /// HTTP status code (or protocol equivalent).
    pub status: u16,
    /// Content length of this response (may differ from total for ranges).
    pub content_length: Option<u64>,
    /// The byte range actually returned, if this is a partial response.
    /// For a 206 response, this is parsed from Content-Range.
    pub content_range: Option<ContentRange>,
    /// Remote identity from response headers.
    pub remote_identity: RemoteIdentity,
    /// The body stream.
    pub body: ByteStream,
}

/// Parsed Content-Range header value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    /// Start byte offset (inclusive).
    pub start: u64,
    /// End byte offset (inclusive).
    pub end: u64,
    /// Total size, if known.
    pub total: Option<u64>,
}

/// Transport trait that protocol implementations must satisfy.
///
/// # Design
///
/// The trait is object-safe and `Send + Sync` to allow sharing across async
/// tasks. Each method returns a boxed future for object safety.
///
/// We use explicit `Pin<Box<dyn Future>>` returns instead of `async fn` in
/// the trait to maintain object safety for dynamic dispatch (`dyn Transport`).
pub trait Transport: Send + Sync {
    /// Probe the remote resource to determine size, range support, and identity.
    fn probe(
        &self,
        url: &Url,
        headers: &[(String, String)],
    ) -> Pin<Box<dyn std::future::Future<Output = Result<ProbeResult, TransportError>> + Send + '_>>;

    /// Fetch bytes from the resource, optionally with a byte range.
    ///
    /// If `range` is `Some((start, end))`, the transport should send a Range
    /// header and validate the response. If the server returns 200 instead
    /// of 206, it must be detected and reported.
    ///
    /// The `identity` parameter contains the validators from the initial probe.
    /// If the server returns different validators, the transport must detect
    /// the mismatch.
    fn fetch(
        &self,
        url: &Url,
        headers: &[(String, String)],
        range: Option<(u64, u64)>,
        identity: &RemoteIdentity,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<FetchResult, TransportError>> + Send + '_>>;
}
