//! Core types shared across the Okytes engine.
//!
//! These types represent the fundamental data structures for download jobs,
//! remote resource identity, and configuration. All types derive `Serialize`
//! and `Deserialize` for persistence.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

/// Unique identifier for a download job.
pub type JobId = Uuid;

/// Unique identifier for a download segment within a job.
pub type SegmentId = Uuid;

/// Identifies the remote resource version for resume validation.
///
/// # Invariant
///
/// Resume is only permitted if the current remote identity matches the one
/// stored at download start. Strong ETags are required for `If-Range`; weak
/// ETags (`W/...`) are never used for byte-range resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RemoteIdentity {
    /// Strong ETag value, if provided. Weak ETags are excluded.
    pub etag: Option<String>,
    /// Last-Modified header value, if provided.
    pub last_modified: Option<String>,
    /// Content-Length header value, if provided.
    pub content_length: Option<u64>,
}

impl RemoteIdentity {
    /// Creates a new empty remote identity.
    pub fn empty() -> Self {
        Self {
            etag: None,
            last_modified: None,
            content_length: None,
        }
    }

    /// Returns true if this identity has any validators set.
    pub fn has_validators(&self) -> bool {
        self.etag.is_some() || self.last_modified.is_some()
    }

    /// Returns the strong ETag suitable for `If-Range`, or `None`.
    ///
    /// # Invariant
    ///
    /// A weak ETag (starting with `W/`) is never returned. This ensures
    /// weak ETags are never used in `If-Range` headers.
    pub fn strong_etag(&self) -> Option<&str> {
        self.etag.as_deref().filter(|e| {
            e.len() >= 2
                && e.starts_with('"')
                && e.ends_with('"')
                && e.as_bytes()[1..e.len() - 1]
                    .iter()
                    .all(|&b| b == 0x21 || (b >= 0x23 && b != 0x7f))
        })
    }

    /// Checks if a newly observed remote identity is compatible with this one.
    ///
    /// Returns `true` if the identities match (safe to resume), or if
    /// the original had no validators (first request).
    pub fn is_compatible(&self, other: &RemoteIdentity) -> bool {
        // If we had no validators, anything is compatible (first fetch).
        if !self.has_validators() && self.content_length.is_none() {
            return true;
        }

        // If ETags are present on both, they must match exactly.
        if let Some(ours) = &self.etag {
            if other.etag.as_ref() != Some(ours) {
                return false;
            }
        }

        // If Last-Modified is present on both, they must match.
        if let Some(ours) = &self.last_modified {
            if other.last_modified.as_ref() != Some(ours) {
                return false;
            }
        }

        // If Content-Length is present on both, they must match.
        if let Some(ours) = self.content_length {
            if other.content_length != Some(ours) {
                return false;
            }
        }

        true
    }
}

/// Strategy for handling filename conflicts at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, utoipa::ToSchema)]
pub enum ConflictStrategy {
    /// Rename the new file with a numeric suffix (e.g., `file (1).zip`).
    #[default]
    Rename,
    /// Overwrite the existing file.
    Overwrite,
    /// Fail the download if the file already exists.
    Fail,
}

/// Parameters for creating a new download job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadRequest {
    /// The URL to download from.
    pub url: Url,
    /// Directory to save the downloaded file in.
    pub output_dir: PathBuf,
    /// Optional filename override. If `None`, derived from Content-Disposition
    /// or URL, with sanitization.
    pub filename: Option<String>,
    /// Optional expected SHA-256 hash for integrity verification.
    pub expected_hash: Option<String>,
    /// Strategy for handling filename conflicts.
    #[serde(default)]
    pub conflict_strategy: ConflictStrategy,
    /// Optional headers to send with the request.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Optional priority (higher = sooner). Default is 0.
    #[serde(default)]
    pub priority: i32,
}

/// Persistent metadata for a download job.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct JobRecord {
    /// Opaque executor identifier. Core does not interpret protocol internals.
    #[serde(default = "http_protocol")]
    pub protocol: String,
    /// Browser request id for durable idempotency, never a credential.
    #[serde(default)]
    #[schema(value_type=Option<String>, format=Uuid)]
    pub capture_id: Option<JobId>,
    /// Requires memory-only browser session headers; fail after session loss.
    #[serde(default)]
    pub requires_session: bool,
    /// Use a sanitized server filename suggestion during the initial probe.
    #[serde(default)]
    pub use_server_filename: bool,
    /// Computed SHA-256 recorded before publication, for crash replay.
    #[serde(default)]
    pub verified_sha256: Option<String>,
    /// Unique job identifier.
    #[schema(value_type=String, format=Uuid)]
    pub id: JobId,
    /// The download URL.
    #[schema(value_type = String)]
    pub url: Url,
    /// Final output path (after sanitization and conflict resolution).
    #[schema(value_type = String)]
    pub output_path: PathBuf,
    /// Path to the partial/temp file during download.
    #[schema(value_type = String)]
    pub partial_path: PathBuf,
    /// Total file size, if known.
    pub total_size: Option<u64>,
    /// Remote identity for resume validation.
    pub remote_identity: RemoteIdentity,
    /// Current job state.
    pub state: crate::state::JobState,
    /// Expected SHA-256 hash, if provided.
    pub expected_hash: Option<String>,
    /// Conflict strategy.
    pub conflict_strategy: ConflictStrategy,
    /// Custom headers for this job.
    #[serde(skip)]
    pub headers: Vec<(String, String)>,
    /// Priority (higher = sooner).
    pub priority: i32,
    /// Bytes downloaded so far (sum of completed segment bytes).
    pub bytes_downloaded: u64,
    /// When the job was created.
    pub created_at: DateTime<Utc>,
    /// When the job was last updated.
    pub updated_at: DateTime<Utc>,
    /// Number of retry attempts so far.
    pub retry_count: u32,
    /// Human-readable error message if failed.
    pub error_message: Option<String>,
}

/// Default identifier for existing HTTP job records.
pub fn http_protocol() -> String {
    "http".into()
}

/// Persistent metadata for a download segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentRecord {
    /// Unique segment identifier.
    pub id: SegmentId,
    /// The job this segment belongs to.
    pub job_id: JobId,
    /// Start byte offset (inclusive).
    pub start: u64,
    /// End byte offset (inclusive).
    pub end: u64,
    /// Bytes downloaded in this segment so far.
    pub bytes_downloaded: u64,
    /// Current segment state.
    pub state: crate::state::SegmentState,
    /// When this segment was last checkpointed.
    pub last_checkpoint: DateTime<Utc>,
}

impl SegmentRecord {
    /// Returns the total byte length of this segment.
    pub fn length(&self) -> u64 {
        self.end - self.start + 1
    }

    /// Returns the current write offset within the file for this segment.
    pub fn current_offset(&self) -> u64 {
        self.start + self.bytes_downloaded
    }

    /// Returns true if this segment has been fully downloaded.
    pub fn is_complete(&self) -> bool {
        self.bytes_downloaded >= self.length()
    }
}

fn default_connection_cap() -> usize {
    16
}

/// Configuration for the download engine.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EngineConfig {
    /// Live payload limits and recurring queue/bandwidth windows.
    #[serde(default)]
    pub traffic: crate::traffic::TrafficPolicy,
    /// Fixed worker count for reproducible controls; null enables adaptation.
    #[serde(default)]
    pub fixed_concurrency: Option<usize>,
    /// Maximum concurrent downloads.
    pub max_concurrent_jobs: usize,
    /// Maximum segments per download.
    pub max_segments_per_job: usize,
    /// Maximum connections per origin (host:port).
    pub max_connections_per_origin: usize,
    /// Aggregate transport connections, including probes and redirected bodies.
    #[serde(default = "default_connection_cap")]
    pub max_connections_total: usize,
    /// Minimum segment size in bytes. Segments smaller than this will not be
    /// split further.
    pub min_segment_size: u64,
    /// Maximum segment size in bytes.
    pub max_segment_size: u64,
    /// Connection timeout in seconds.
    pub connect_timeout_secs: u64,
    /// Read timeout in seconds (per chunk).
    pub read_timeout_secs: u64,
    /// Maximum retry attempts per segment.
    pub max_retries: u32,
    /// Base delay for exponential backoff in milliseconds.
    pub retry_base_delay_ms: u64,
    /// Maximum backoff delay in milliseconds.
    pub retry_max_delay_ms: u64,
    /// Default output directory.
    #[schema(value_type = String)]
    pub default_output_dir: PathBuf,
    /// Database file path.
    #[schema(value_type = String)]
    pub database_path: PathBuf,
    /// EWMA smoothing factor for throughput measurement (0.0–1.0).
    pub ewma_alpha: f64,
    /// Bounded channel capacity for internal events.
    pub channel_capacity: usize,
    /// Bounded buffer size for write operations in bytes.
    pub write_buffer_size: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            fixed_concurrency: None,
            traffic: Default::default(),
            max_concurrent_jobs: 4,
            max_segments_per_job: 8,
            max_connections_per_origin: 6,
            max_connections_total: default_connection_cap(),
            min_segment_size: 512 * 1024,       // 512 KiB
            max_segment_size: 64 * 1024 * 1024, // 64 MiB
            connect_timeout_secs: 30,
            read_timeout_secs: 60,
            max_retries: 10,
            retry_base_delay_ms: 500,
            retry_max_delay_ms: 60_000,
            default_output_dir: PathBuf::from("."),
            database_path: PathBuf::from("okytes.db"),
            ewma_alpha: 0.3,
            channel_capacity: 256,
            write_buffer_size: 8 * 1024 * 1024, // 8 MiB
        }
    }
}

impl EngineConfig {
    /// Reject invalid settings and allocations beyond the supported bounds.
    pub fn validate(&self) -> crate::error::EngineResult<()> {
        self.traffic.validate()?;
        let c = self;
        if c.fixed_concurrency
            .is_some_and(|n| n == 0 || n > c.max_segments_per_job)
            || c.max_concurrent_jobs == 0
            || c.max_concurrent_jobs > 32
            || c.max_segments_per_job == 0
            || c.max_segments_per_job > 128
            || c.max_connections_per_origin == 0
            || c.max_connections_per_origin > 128
            || c.max_connections_total == 0
            || c.max_connections_total > 128
            || c.min_segment_size == 0
            || c.max_segment_size < c.min_segment_size
            || c.max_segment_size > 64 * 1024 * 1024
            || c.write_buffer_size == 0
            || c.write_buffer_size > 8 * 1024 * 1024
            || c.channel_capacity == 0
            || c.channel_capacity > 4096
            || !(0.0..=1.0).contains(&c.ewma_alpha)
            || c.connect_timeout_secs == 0
            || c.connect_timeout_secs > 3600
            || c.read_timeout_secs == 0
            || c.read_timeout_secs > 3600
            || c.max_retries > 100
            || c.retry_base_delay_ms > c.retry_max_delay_ms
            || c.retry_max_delay_ms > 3_600_000
        {
            return Err(crate::error::EngineError::Config(
                "configuration exceeds supported resource bounds".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_quoted_strong_etags_are_usable_and_vanished_validators_fail() {
        let mut identity = RemoteIdentity {
            etag: None,
            last_modified: None,
            content_length: Some(10),
        };
        for tag in ["W/\"weak\"", " W/\"weak\"", "unquoted", "\"bad\"quote\""] {
            identity.etag = Some(tag.into());
            assert!(identity.strong_etag().is_none());
        }
        identity.etag = Some("\"strong\"".into());
        assert_eq!(identity.strong_etag(), Some("\"strong\""));
        assert!(!identity.is_compatible(&RemoteIdentity {
            etag: None,
            last_modified: None,
            content_length: Some(10)
        }));
    }
    #[test]
    fn configuration_rejects_zero_and_unbounded_allocations() {
        let mut config = EngineConfig {
            max_concurrent_jobs: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
        config.max_concurrent_jobs = 1;
        config.channel_capacity = usize::MAX;
        assert!(config.validate().is_err());
    }
}
