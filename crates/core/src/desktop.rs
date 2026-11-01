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
    /// Ask before removing an item from history.
    #[serde(default = "default_true")]
    pub confirm_remove: bool,
    /// Remember whether removal also deletes the downloaded file.
    #[serde(default)]
    pub remove_files: bool,
    /// Enable decorative motion and animated transitions.
    #[serde(default = "default_true")]
    pub animations: bool,
    /// Comfortable or compact interface density.
    #[serde(default = "default_density")]
    pub density: String,
    /// User-selected accent: blue, violet, teal or rose.
    #[serde(default = "default_accent")]
    pub accent: String,
    /// Keyboard shortcut for adding downloads.
    #[serde(default = "default_add_shortcut")]
    pub shortcut_add: String,
    /// Keyboard shortcut for focusing search.
    #[serde(default = "default_search_shortcut")]
    pub shortcut_search: String,
    /// Keyboard shortcut for opening shortcut settings.
    #[serde(default = "default_help_shortcut")]
    pub shortcut_help: String,
    /// Open a download's own details window when its transfer starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub details_on_start: Option<bool>,
    /// Per-download actions run once after completion; at most 128 entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub completion: Option<Vec<CompletionAction>>,
}

/// What to do when one download completes. Removed after it runs.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CompletionAction {
    /// Job UUID.
    pub id: String,
    /// Open the file with its default application.
    pub open_file: bool,
    /// Reveal the file in the system file manager.
    pub open_folder: bool,
    /// Show a notification; overrides the global notification preference.
    pub notify: bool,
    /// Upload the file to a connected provider: gdrive, dropbox or onedrive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub upload: Option<String>,
}

fn default_true() -> bool {
    true
}
fn default_density() -> String {
    "comfortable".into()
}
fn default_accent() -> String {
    "blue".into()
}
fn default_add_shortcut() -> String {
    "Shift+N".into()
}
fn default_search_shortcut() -> String {
    "Shift+F".into()
}
fn default_help_shortcut() -> String {
    "Shift+?".into()
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
    /// Remove history, partial data and the published file/directory.
    RemoveFiles,
    /// Rename an idle job's destination filename.
    Rename(String),
    /// Set queue priority, between -100 and 100.
    Priority(i32),
    /// Replace an expired or broken link on a paused or failed job, keeping
    /// progress when the new link serves the same file.
    Refresh(String),
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
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// RFC 3339 last state/progress timestamp; terminal jobs use completion time.
    pub updated_at: String,
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

/// One transient network request serving part of a persisted segment.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
pub struct Connection {
    /// Engine-assigned connection number.
    pub id: u32,
    /// First byte of the persisted segment this request serves.
    pub segment_start: f64,
    /// First requested byte.
    pub start: f64,
    /// Last requested byte; null when the resource size is unknown.
    pub end: Option<f64>,
    /// Bytes received on this connection.
    pub bytes: f64,
    /// Sampled bytes per second.
    pub speed: f64,
    /// Retry attempts made by this request.
    pub retries: u32,
    /// connecting, downloading, stalled or retrying.
    pub state: String,
    /// Milliseconds since the request started.
    pub age_ms: f64,
}

/// Live single-job diagnostics. Session values are null when not yet sampled.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
pub struct Telemetry {
    /// Durable job snapshot.
    pub job: Job,
    /// Active network requests; empty when idle.
    pub connections: Vec<Connection>,
    /// Server honoured byte ranges during this session's probe.
    pub range_support: Option<bool>,
    /// Last-Modified validator, when the server provided one.
    pub last_modified: Option<String>,
    /// Average bytes per second since this session started transferring.
    pub average_speed: Option<f64>,
    /// Highest sampled bytes per second this session.
    pub peak_speed: Option<f64>,
    /// Current adaptive or fixed connection limit.
    pub concurrency_limit: Option<u32>,
    /// A publisher SHA-256 was supplied for verification.
    pub hash_expected: bool,
    /// Bytes covered by a completed SHA-256 computation.
    pub verified_bytes: f64,
}

/// Website crawl request.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
pub struct CrawlInput {
    /// Starting page, http(s).
    pub url: String,
    /// Link levels to follow from the start page, 0–3.
    pub depth: u32,
    /// Only collect files hosted on the starting site.
    pub same_host: bool,
    /// File extensions to collect, e.g. `zip`; empty collects every non-page link.
    pub extensions: Vec<String>,
    /// Pages to visit, 1–200.
    pub max_pages: u32,
}

/// One candidate file link found while crawling.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
pub struct CrawlLink {
    /// Absolute link.
    pub url: String,
    /// Page where the link was found.
    pub page: String,
    /// Lowercase file extension, possibly empty.
    pub extension: String,
}

/// Crawl outcome for review before adding downloads.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
pub struct CrawlResult {
    /// Unique candidate links, at most 2,000.
    pub links: Vec<CrawlLink>,
    /// Pages fetched.
    pub pages: u32,
    /// Pages skipped because robots.txt disallows them.
    pub skipped_by_robots: u32,
    /// A page or link limit stopped the crawl early.
    pub truncated: bool,
}

/// One entry from a remote ZIP central directory.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
pub struct ZipEntry {
    /// Path inside the archive, as stored.
    pub name: String,
    /// Uncompressed bytes.
    pub size: f64,
    /// Compressed bytes.
    pub compressed: f64,
    /// ZIP compression method (0 stored, 8 deflate, others shown as numbers).
    pub method: u32,
    /// Entry is encrypted.
    pub encrypted: bool,
    /// Entry is a directory.
    pub directory: bool,
    /// Local modification time, `YYYY-MM-DD HH:MM`, when valid.
    pub modified: Option<String>,
}

/// Remote ZIP listing read without downloading the archive.
#[derive(Debug, Clone, Serialize, Deserialize, TS, utoipa::ToSchema)]
#[ts(export)]
pub struct ZipListing {
    /// Archive size in bytes.
    pub archive_size: f64,
    /// Entries in the central directory.
    pub total_entries: f64,
    /// More entries exist than were listed (at most 5,000 are returned).
    pub truncated: bool,
    /// Archive comment, when present.
    pub comment: Option<String>,
    /// Listed entries in directory order.
    pub entries: Vec<ZipEntry>,
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
    /// List a remote ZIP's contents using range requests, without a job.
    fn inspect_zip(&self, url: String) -> Reply<'_, ZipListing> {
        Box::pin(async move {
            let _ = url;
            Err(crate::error::EngineError::Config(
                "ZIP preview is unavailable".into(),
            ))
        })
    }
    /// Expand an M3U, PLS or XSPF playlist into its listed links.
    fn expand_playlist(&self, url: String) -> Reply<'_, Vec<String>> {
        Box::pin(async move {
            let _ = url;
            Err(crate::error::EngineError::Config(
                "playlist expansion is unavailable".into(),
            ))
        })
    }
    /// Collect candidate file links from a website, without creating jobs.
    fn crawl(&self, input: CrawlInput) -> Reply<'_, CrawlResult> {
        Box::pin(async move {
            let _ = input;
            Err(crate::error::EngineError::Config(
                "website crawling is unavailable".into(),
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
    /// Get one job with live connection telemetry from an embedded engine.
    fn telemetry(&self, id: String) -> Reply<'_, Telemetry> {
        Box::pin(async move {
            let _ = id;
            Err(crate::error::EngineError::Config(
                "live telemetry is unavailable".into(),
            ))
        })
    }
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
