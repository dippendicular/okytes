//! Versioned desktop boundary. Persisted DTOs omit secrets; request-local file
//! credentials are redacted and passed only to the engine memory session.
use crate::error::EngineResult;
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};
use ts_rs::TS;

/// Object-safe asynchronous service result.
pub type Reply<'a, T> = Pin<Box<dyn Future<Output = EngineResult<T>> + Send + 'a>>;

/// Advanced single-job creation shared by embedded and daemon clients.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Submit {
    /// Request-local file-protocol credentials; never stored in history.
    #[serde(default)]
    pub credentials: Option<FileCredentials>,
    /// Supported transfer URL without userinfo.
    pub url: String,
    /// Absolute override; null uses service preferences.
    pub destination: Option<String>,
    /// Optional safe basename.
    pub filename: Option<String>,
    /// Optional publisher SHA-256.
    pub sha256: Option<String>,
    /// rename, fail or overwrite.
    pub conflict: String,
}

/// Memory-only file login and an optional explicitly confirmed SSH fingerprint.
#[derive(Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct FileCredentials {
    /// Remote account name.
    pub username: String,
    /// Password, retained only for this process session.
    pub password: String,
    /// Exact SHA256 fingerprint the user has verified and confirmed.
    pub confirmed_host_key: Option<String>,
}
impl std::fmt::Debug for FileCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FileCredentials(<redacted>)")
    }
}
impl FileCredentials {
    /// Convert to the engine's existing bounded memory-only session handoff.
    pub fn into_headers(self) -> Vec<(String, String)> {
        let mut headers = vec![
            ("okytes-user".into(), self.username),
            ("okytes-password".into(), self.password),
        ];
        if let Some(key) = self.confirmed_host_key {
            headers.push(("okytes-host-key".into(), key));
        }
        headers
    }
}

/// A manifest-advertised quality, audio or subtitle selection.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
pub struct MediaChoice {
    /// Stable selection identifier.
    pub id: String,
    /// Quality, language and codec details reported by the manifest.
    pub label: String,
    /// Explicit media source URL for this selection.
    pub url: String,
    /// video, audio or subtitles.
    pub kind: String,
    /// Suggested output extension for the supported assembly path.
    pub extension: String,
}

/// Persisted presentation and queue settings.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
pub struct Settings {
    /// Shared runtime bandwidth and queue schedule policy.
    #[serde(default)]
    pub traffic: crate::traffic::TrafficPolicy,
    /// Schema version, currently one.
    pub version: u32,
    /// Absolute default destination directory.
    pub destination: String,
    /// Concurrent jobs, from one to 32.
    pub concurrency: u32,
    /// Light, dark or system.
    pub theme: String,
    /// Native completion/failure notifications.
    pub notifications: bool,
}

/// Validated batch creation input, limited to 100 URLs.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
pub struct AddInput {
    /// URLs, maximum 8192 bytes each.
    pub urls: Vec<String>,
    /// Optional per-batch destination override.
    pub destination: Option<String>,
    /// Explicit permission to add duplicate URLs.
    pub allow_duplicates: bool,
}

/// Queue control request; payload validation belongs to the service.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
#[serde(tag = "kind", content = "value")]
pub enum Action {
    /// Pause and wait for workers to release ownership.
    Pause,
    /// Resume a paused or failed job.
    Resume,
    /// Cancel and clean up partial data after workers stop.
    Cancel,
    /// Create a fresh download from the same URL.
    Restart,
    /// Remove history and partial data, preserving completed files.
    Remove,
    /// Rename an idle job's destination filename.
    Rename(String),
    /// Set queue priority, between -100 and 100.
    Priority(i32),
}

/// Display metadata and durable progress for one job.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
pub struct Job {
    /// Opaque protocol identifier; details are read through its app contract.
    #[serde(default = "crate::types::http_protocol")]
    pub protocol: String,
    /// UUID.
    pub id: String,
    /// Filename.
    pub name: String,
    /// Source URL explicitly entered by the user.
    pub url: String,
    /// Final destination.
    pub destination: String,
    /// Persisted state name.
    pub state: String,
    /// Durable downloaded bytes.
    pub bytes: f64,
    /// Resource size, if known.
    pub total: Option<f64>,
    /// Measured durable bytes per second.
    pub speed: f64,
    /// Priority.
    pub priority: i32,
    /// Retry count.
    pub retries: u32,
    /// Live transfer tasks; includes origin admission waits and retry backoff.
    pub workers: u32,
    /// Last actionable engine error.
    pub error: Option<String>,
    /// Strong validator, when available.
    pub etag: Option<String>,
    /// Last successful HTTP response status (session diagnostics).
    pub response_status: Option<u16>,
    /// Time to response headers, including origin admission and redirects.
    pub response_latency_ms: Option<f64>,
    /// Segment ownership and checkpoints.
    pub segments: Vec<Segment>,
}

/// Segment diagnostic; byte offsets never exceed the engine's u64 bounds.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
pub struct Segment {
    /// Inclusive first byte.
    pub start: f64,
    /// Inclusive last byte.
    pub end: f64,
    /// Durable bytes.
    pub bytes: f64,
    /// Segment state name.
    pub state: String,
}

/// Bounded snapshot page; clients replace pages after a missed update.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
pub struct Snapshot {
    /// Monotonic session revision, serialized as a string.
    pub revision: String,
    /// At most 256 jobs.
    pub jobs: Vec<Job>,
    /// Next offset if more history exists.
    pub next: Option<u32>,
}

/// GUI-independent management contract for an embedded engine or future daemon.
pub trait DesktopService: Send + Sync {
    /// Restore a file login in memory; no password enters durable job metadata.
    fn authorize_file(&self, id: String, credentials: FileCredentials) -> Reply<'_, ()> {
        Box::pin(async move {
            let _ = (id, credentials);
            Err(crate::error::EngineError::Config(
                "file authorization is unavailable".into(),
            ))
        })
    }
    /// Inspect supported VOD tracks without creating a job or persisting credentials.
    fn inspect_media(&self, url: String) -> Reply<'_, Vec<MediaChoice>> {
        Box::pin(async move {
            let _ = url;
            Err(crate::error::EngineError::Config(
                "media inspection is unavailable".into(),
            ))
        })
    }
    /// Add one advanced request, preserving CLI options across backends.
    fn submit(&self, input: Submit) -> Reply<'_, String>;
    /// Pause the runnable queue with one admission lock and wait for all workers.
    fn pause_all(&self) -> Reply<'_, ()>;
    /// Add a bounded batch; reject duplicates unless explicitly allowed.
    fn add(&self, input: AddInput) -> Reply<'_, Vec<String>>;
    /// Read a bounded page; progress is coalesced by the implementation.
    fn list(&self, offset: u32) -> Reply<'_, Snapshot>;
    /// Get one job and its diagnostics.
    fn get(&self, id: String) -> Reply<'_, Job>;
    /// Apply a validated control request.
    fn control(&self, id: String, action: Action) -> Reply<'_, ()>;
    /// Read versioned settings.
    fn settings(&self) -> Reply<'_, Settings>;
    /// Persist validated settings and apply them to queue admission.
    fn set_settings(&self, settings: Settings) -> Reply<'_, ()>;
    /// Subscribe to bounded engine events; a lagged receiver must reload snapshots.
    fn subscribe(&self)
        -> Reply<'_, tokio::sync::broadcast::Receiver<crate::service::EngineEvent>>;
}
