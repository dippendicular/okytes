//! Service trait for the download engine.
//!
//! Defines the high-level API that frontends (CLI, desktop, daemon) use to
//! interact with the engine. This trait will be the boundary between the
//! engine and its consumers from M2 onward.

use crate::error::EngineResult;
use crate::state::JobState;
use crate::types::{DownloadRequest, JobId, JobRecord};

/// Progress information for a running download.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DownloadProgress {
    /// Job identifier.
    pub job_id: JobId,
    /// Current job state.
    pub state: JobState,
    /// Bytes downloaded so far.
    pub bytes_downloaded: u64,
    /// Total file size (if known).
    pub total_size: Option<u64>,
    /// Current throughput in bytes/second.
    pub throughput_bps: f64,
    /// Active segment count.
    pub active_segments: usize,
    /// Completed segment count.
    pub completed_segments: usize,
    /// Total segment count.
    pub total_segments: usize,
}

/// Events emitted by the engine.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum EngineEvent {
    /// Job state changed.
    JobStateChanged {
        job_id: JobId,
        old_state: JobState,
        new_state: JobState,
    },
    /// Download progress updated.
    Progress(DownloadProgress),
    /// Job completed successfully.
    JobCompleted { job_id: JobId },
    /// Job failed with an error.
    JobFailed { job_id: JobId, error: String },
}

/// Service trait defining the public engine API.
///
/// Implementations include the in-process engine (for CLI and embedded desktop)
/// and the daemon client (for remote operation).
pub trait DownloadService: Send + Sync {
    /// Add a new download job. Returns the assigned job ID.
    fn add(
        &self,
        request: DownloadRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = EngineResult<JobId>> + Send + '_>>;

    /// Pause a running download.
    fn pause(
        &self,
        job_id: JobId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = EngineResult<()>> + Send + '_>>;

    /// Resume a paused or failed download.
    fn resume(
        &self,
        job_id: JobId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = EngineResult<()>> + Send + '_>>;

    /// Cancel a download (removes from queue, retains history).
    fn cancel(
        &self,
        job_id: JobId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = EngineResult<()>> + Send + '_>>;

    /// List the first bounded page (up to 256 jobs), optionally filtered by state.
    fn list(
        &self,
        filter: Option<JobState>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = EngineResult<Vec<JobRecord>>> + Send + '_>,
    >;

    /// Get details for a specific job.
    fn status(
        &self,
        job_id: JobId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = EngineResult<JobRecord>> + Send + '_>>;

    /// Subscribe to engine events. Returns a receiver for bounded events.
    fn subscribe(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = EngineResult<tokio::sync::broadcast::Receiver<EngineEvent>>,
                > + Send
                + '_,
        >,
    >;
}
