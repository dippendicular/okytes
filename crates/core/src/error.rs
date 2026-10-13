//! Error types for the Okytes core engine.
//!
//! Uses `thiserror` for structured, actionable error messages. No `unwrap()`,
//! `expect()`, or `panic!` outside tests.

use std::path::PathBuf;

use thiserror::Error;
use url::Url;
use uuid::Uuid;

/// Top-level errors from the download engine.
#[derive(Debug, Error)]
pub enum EngineError {
    /// A transport-level error occurred during download.
    #[error("transport error for job {job_id}: {source}")]
    Transport {
        job_id: Uuid,
        #[source]
        source: Box<TransportError>,
    },

    /// A persistence-level error occurred.
    #[error("persistence error: {0}")]
    Persistence(#[from] PersistenceError),

    /// The remote resource identity changed between requests.
    #[error("remote identity mismatch for job {job_id}: {detail}")]
    IdentityMismatch { job_id: Uuid, detail: String },

    /// Hash verification failed after download completion.
    #[error("integrity check failed for job {job_id}: expected {expected}, got {actual}")]
    IntegrityMismatch {
        job_id: Uuid,
        expected: String,
        actual: String,
    },

    /// An invalid state transition was attempted.
    #[error("invalid state transition for job {job_id}: {detail}")]
    InvalidTransition { job_id: Uuid, detail: String },

    /// The requested job was not found.
    #[error("job not found: {0}")]
    JobNotFound(Uuid),

    /// A file I/O error occurred.
    #[error("file I/O error at {path}: {source}")]
    FileIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The atomic rename from partial to final failed.
    #[error("atomic rename failed from {from} to {to}: {source}")]
    AtomicRename {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The scheduler rejected the operation.
    #[error("scheduler error: {0}")]
    Scheduler(String),

    /// A configuration error.
    #[error("configuration error: {0}")]
    Config(String),
}

/// Errors from transport implementations.
#[derive(Debug, Error)]
pub enum TransportError {
    /// Transport configuration could not be constructed.
    #[error("transport configuration: {0}")]
    Configuration(String),
    /// HTTP request failed.
    #[error("HTTP request failed for {url}: {detail}")]
    Request { url: Box<Url>, detail: String },

    /// Server returned an unexpected status code.
    #[error("unexpected status {status} for {url}: {detail}")]
    UnexpectedStatus {
        url: Box<Url>,
        status: u16,
        detail: String,
    },

    /// Server does not support range requests.
    #[error("range requests not supported by {url}")]
    RangeNotSupported { url: Box<Url> },

    /// Server returned 200 to a range request (full body).
    #[error("server returned 200 (full body) to range request for {url}")]
    RangeIgnored { url: Box<Url> },

    /// Content-Range header is missing or malformed.
    #[error("invalid Content-Range from {url}: {detail}")]
    InvalidContentRange { url: Box<Url>, detail: String },

    /// Server returned wrong byte range.
    #[error("wrong byte range from {url}: expected {expected}, got {actual}")]
    WrongRange {
        url: Box<Url>,
        expected: String,
        actual: String,
    },

    /// Connection timed out.
    #[error("connection timeout for {url}")]
    Timeout { url: Box<Url> },

    /// TLS/certificate error.
    #[error("TLS error for {url}: {detail}")]
    Tls { url: Box<Url>, detail: String },

    /// Too many redirects.
    #[error("too many redirects for {url}")]
    TooManyRedirects { url: Box<Url> },

    /// Server sent a throttle response (429 or 503).
    #[error("throttled by {url}: retry after {retry_after_secs:?}s")]
    Throttled {
        url: Box<Url>,
        retry_after_secs: Option<u64>,
    },

    /// The URL has expired or is no longer valid.
    #[error("URL expired or invalid: {url}")]
    UrlExpired { url: Box<Url> },

    /// Data corruption detected in received chunk.
    #[error("data corruption detected from {url}")]
    DataCorruption { url: Box<Url> },

    /// Network I/O error.
    #[error("network I/O error for {url}: {source}")]
    Io {
        url: Box<Url>,
        #[source]
        source: std::io::Error,
    },
}

/// Errors from the persistence layer.
#[derive(Debug, Error)]
pub enum PersistenceError {
    /// SQLite operation failed.
    #[error("database error: {0}")]
    Database(String),

    /// Serialization/deserialization failed.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// Migration failed.
    #[error("migration error: {0}")]
    Migration(String),

    /// The database is in an inconsistent state.
    #[error("inconsistent state: {0}")]
    Inconsistent(String),
}

/// A specialized `Result` type for engine operations.
pub type EngineResult<T> = Result<T, EngineError>;
