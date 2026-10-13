//! Persistence boundary; callers checkpoint only after durable data writes.
use crate::error::EngineError;
use crate::state::JobState;
use crate::types::{JobId, JobRecord, SegmentRecord};
/// Trait for the persistence store, implemented by the persistence crate.
///
/// This allows the engine to remain decoupled from SQLite specifics.
pub trait JobStore: Send + Sync {
    /// Atomically persist last error and retry diagnostics without changing state.
    fn record_diagnostic(
        &self,
        id: JobId,
        error: Option<String>,
        retry: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EngineError>> + Send + '_>>;
    /// Atomically transition state without losing concurrent control requests.
    fn transition_job(
        &self,
        id: JobId,
        state: JobState,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<JobRecord, EngineError>> + Send + '_>,
    >;
    /// Save or update a job record.
    fn save_job(
        &self,
        job: &JobRecord,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EngineError>> + Send + '_>>;

    /// Load a job record by ID.
    fn load_job(
        &self,
        id: JobId,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JobRecord>, EngineError>> + Send + '_>,
    >;

    /// List the first bounded page (up to 256 jobs), optionally filtered by state.
    fn list_jobs(
        &self,
        filter: Option<JobState>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<JobRecord>, EngineError>> + Send + '_>,
    >;

    /// Save or update segments for a job.
    fn save_segments(
        &self,
        segments: &[SegmentRecord],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EngineError>> + Send + '_>>;

    /// Load segments for a job.
    fn load_segments(
        &self,
        job_id: JobId,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<SegmentRecord>, EngineError>> + Send + '_>,
    >;

    /// Checkpoint a segment's progress.
    ///
    /// # Invariant
    ///
    /// This must be called **after** data has been written and `sync_data` has
    /// been called on the file. The store commits the checkpoint to SQLite
    /// only after it is durable on disk.
    fn checkpoint_segment(
        &self,
        segment: &SegmentRecord,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EngineError>> + Send + '_>>;

    /// Delete a job and its segments.
    fn delete_job(
        &self,
        id: JobId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EngineError>> + Send + '_>>;
}
