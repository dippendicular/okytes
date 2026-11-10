//! Transfer orchestration. Workers own disjoint ranges and yield at bounded
//! request boundaries; a free worker takes over the unreserved tail of the
//! largest unfinished range. Data is synced before checkpoint commits. Control state
//! is authoritative in SQLite, allowing another CLI to pause a foreground runner.
pub use crate::store::JobStore;
use crate::{
    error::{EngineError, EngineResult, TransportError},
    range::{allocate_ranges, validate_ranges, Lease},
    scheduler::{retry_delay, EwmaThroughput},
    service::{DownloadProgress, DownloadService, EngineEvent},
    state::{JobState, SegmentState},
    transport::Transport,
    types::{
        ConflictStrategy, DownloadRequest, EngineConfig, JobId, JobRecord, RemoteIdentity,
        SegmentRecord,
    },
};
use chrono::Utc;
use futures::{stream::FuturesUnordered, StreamExt};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{broadcast, Semaphore},
};
use url::Url;
use uuid::Uuid;
type ServiceFuture<'a, T> = Pin<Box<dyn Future<Output = EngineResult<T>> + Send + 'a>>;

/// Headless engine; the transport owns redirect-aware origin connection permits.
pub struct Engine {
    config: EngineConfig,
    transport: Arc<dyn Transport>,
    store: Arc<dyn JobStore>,
    files: Arc<dyn crate::files::PartialFiles>,
    jobs: Semaphore,
    active: Mutex<HashSet<JobId>>,
    events: broadcast::Sender<EngineEvent>,
    responses: Mutex<HashMap<JobId, (u16, f64)>>,
    full_progress: Mutex<HashMap<JobId, u64>>,
    workers: Mutex<HashMap<JobId, u32>>,
    sessions: Mutex<HashMap<JobId, Vec<(String, String)>>>,
    traffic: Arc<crate::traffic::TrafficController>,
    fair: Arc<crate::fairness::FairGate>,
    telemetry: crate::telemetry::Telemetry,
}
struct Active<'a> {
    ids: &'a Mutex<HashSet<JobId>>,
    id: JobId,
    full_progress: &'a Mutex<HashMap<JobId, u64>>,
    traffic: &'a crate::traffic::TrafficController,
    telemetry: &'a crate::telemetry::Telemetry,
}
struct Worker<'a> {
    counts: &'a Mutex<HashMap<JobId, u32>>,
    id: JobId,
}
/// A failed request that still committed this much does not use up the retry
/// budget: a link that drops repeatedly but keeps moving finishes the file.
/// The floor keeps requests per quantum bounded.
const RETRY_PROGRESS_BYTES: u64 = 64 * 1024;
/// A resumable request with no body bytes for this long is replaced by a new
/// one from the last durable offset instead of waiting out the read timeout.
const STALL_SECS: u64 = 20;
/// One in-flight request: its live byte ownership and the last record the
/// store holds. The record lock orders a worker's checkpoints against a split
/// of its tail; the scheduler only ever tries it, because workers make progress
/// solely while the scheduler polls them.
struct Flight {
    lease: Lease,
    durable: tokio::sync::Mutex<SegmentRecord>,
}
/// Per-transfer counters written by workers and drained by the scheduler tick.
#[derive(Default)]
struct Pulse {
    /// Bytes accepted from the network: the throughput signal.
    received: AtomicU64,
    /// Bytes checkpointed: reported progress.
    bytes: AtomicU64,
    /// Retryable request failures: the health signal.
    failures: AtomicU32,
}
impl Drop for Worker<'_> {
    fn drop(&mut self) {
        if let Ok(mut counts) = self.counts.lock() {
            if let Some(count) = counts.get_mut(&self.id) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    counts.remove(&self.id);
                }
            }
        }
    }
}
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.traffic.release(self.id);
        self.telemetry.stopped(self.id);
        if let Ok(mut ids) = self.ids.lock() {
            ids.remove(&self.id);
        }
        if let Ok(mut progress) = self.full_progress.lock() {
            progress.remove(&self.id);
        }
    }
}
fn io(path: &Path, source: std::io::Error) -> EngineError {
    EngineError::FileIo {
        path: path.into(),
        source,
    }
}

impl Engine {
    /// Construct an engine and validate its resource limits.
    pub fn new(
        config: EngineConfig,
        transport: Arc<dyn Transport>,
        store: Arc<dyn JobStore>,
        files: Arc<dyn crate::files::PartialFiles>,
    ) -> EngineResult<Self> {
        config.validate()?;
        let (events, _) = broadcast::channel(config.channel_capacity.max(1));
        Ok(Self {
            fair: crate::fairness::FairGate::new(
                config.max_connections_total,
                config.max_connections_per_origin,
            ),
            traffic: Arc::new(crate::traffic::TrafficController::new(
                config.traffic.clone(),
            )?),
            jobs: Semaphore::new(config.max_concurrent_jobs.max(1)),
            config,
            transport,
            store,
            files,
            active: Mutex::new(HashSet::new()),
            events,
            responses: Mutex::new(HashMap::new()),
            full_progress: Mutex::new(HashMap::new()),
            workers: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            telemetry: Default::default(),
        })
    }
    /// Apply persisted payload preferences without replacing running workers.
    pub fn set_traffic(&self, policy: crate::traffic::TrafficPolicy) -> EngineResult<()> {
        self.traffic.set(policy)
    }
    /// Shared aggregate payload admission for independent protocol executors.
    pub fn traffic_controller(&self) -> Arc<crate::traffic::TrafficController> {
        self.traffic.clone()
    }
    /// Bounded event producer for independent job executors.
    pub fn event_sender(&self) -> broadcast::Sender<EngineEvent> {
        self.events.clone()
    }
    /// Current recurring queue gate. Uses UTC wall time and explicit IANA zone.
    pub fn queue_allowed(&self) -> EngineResult<bool> {
        self.traffic.queue_allowed(Utc::now())
    }
    /// Add a browser job using its request UUID for durable idempotency. Only the
    /// authenticated queue owner may call this after checking existing records.
    pub async fn add_captured(&self, request: DownloadRequest, id: JobId) -> EngineResult<JobId> {
        self.add_request(request, Some(id)).await
    }
    /// Replace an expired or broken source link on a stopped job, keeping its
    /// durable progress. With partial data the new link must identify the same
    /// remote file; the normal resume checks then still run on every request.
    pub async fn refresh_source(&self, id: JobId, url: Url) -> EngineResult<()> {
        let mut job = self.status(id).await?;
        if job.protocol != "http" || !matches!(url.scheme(), "http" | "https") {
            return Err(EngineError::Config(
                "link refresh supports HTTP(S) downloads only".into(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(EngineError::Config(
                "use a link without an embedded username or password".into(),
            ));
        }
        if !matches!(job.state, JobState::Paused | JobState::Failed)
            || self.active.lock().map(|a| a.contains(&id)).unwrap_or(true)
        {
            return Err(EngineError::Config(
                "pause the download before replacing its link".into(),
            ));
        }
        // Session cookies were captured for the original site; never send
        // them to a different origin.
        let same_origin = job.url.origin() == url.origin();
        let headers = if same_origin {
            self.session_headers(&job).unwrap_or_default()
        } else {
            vec![]
        };
        let probe = self
            .transport
            .probe(&url, &headers)
            .await
            .map_err(|source| EngineError::Transport {
                job_id: id,
                source: Box::new(source),
            })?;
        let durable: u64 = self
            .store
            .load_segments(id)
            .await?
            .iter()
            .map(|s| s.bytes_downloaded)
            .sum();
        if durable > 0 {
            let identified = job.remote_identity.strong_etag().is_some()
                || (job.remote_identity.last_modified.is_some()
                    && job.remote_identity.content_length.is_some());
            if !identified || !job.remote_identity.is_compatible(&probe.remote_identity) {
                return Err(EngineError::IdentityMismatch {
                    job_id: id,
                    detail: "the new link serves a different file (size, ETag or date differ); use Download again to start over".into(),
                });
            }
        }
        if !same_origin {
            job.requires_session = false;
            self.forget_session(id);
        }
        job.url = url;
        job.error_message = None;
        job.updated_at = Utc::now();
        self.store.save_job(&job).await
    }
    /// Drop memory-only credentials after cancellation, removal or completion.
    pub fn forget_session(&self, id: JobId) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(&id);
        }
    }
    /// Restore an explicitly supplied memory-only file login after process loss.
    pub async fn authorize_file(
        &self,
        id: JobId,
        credentials: crate::desktop::FileCredentials,
    ) -> EngineResult<()> {
        let mut job = self.status(id).await?;
        if !matches!(job.protocol.as_str(), "ftp" | "ftps" | "sftp")
            || job.state.is_active()
            || job.state.is_terminal() && job.state != JobState::Failed
        {
            return Err(EngineError::Config(
                "file login requires an idle resumable file job".into(),
            ));
        }
        if credentials.username.len() > 256
            || credentials.password.len() > 4096
            || credentials
                .confirmed_host_key
                .as_ref()
                .is_some_and(|s| s.len() > 128)
            || credentials
                .clone()
                .into_headers()
                .iter()
                .any(|(_, v)| v.bytes().any(|b| b < 32 || b == 127))
        {
            return Err(EngineError::Config("invalid file credentials".into()));
        }
        job.requires_session = true;
        self.store.save_job(&job).await?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| EngineError::Config("session lock failed".into()))?;
        if !sessions.contains_key(&id) && sessions.len() >= 128 {
            return Err(EngineError::Config("session limit reached".into()));
        }
        sessions.insert(id, credentials.into_headers());
        Ok(())
    }
    fn session_headers(&self, job: &JobRecord) -> EngineResult<Vec<(String, String)>> {
        if !job.requires_session {
            return Ok(vec![]);
        }
        self.sessions
            .lock()
            .map_err(|_| EngineError::Config("session lock failed".into()))?
            .get(&job.id)
            .cloned()
            .ok_or_else(|| {
                EngineError::Config(
                    if matches!(job.protocol.as_str(), "ftp" | "ftps" | "sftp") {
                        "File login expired; enter credentials again to resume"
                    } else {
                        "Browser session expired; capture this download again from the browser"
                    }
                    .into(),
                )
            })
    }
    async fn add_request(
        &self,
        request: DownloadRequest,
        capture_id: Option<JobId>,
    ) -> EngineResult<JobId> {
        if request.headers.len() > 16
            || request
                .headers
                .iter()
                .map(|(n, v)| n.len() + v.len())
                .sum::<usize>()
                > 32768
            || request.headers.iter().any(|(name, value)| {
                !(matches!(
                    name.to_ascii_lowercase().as_str(),
                    "cookie" | "authorization" | "referer" | "user-agent" | "accept"
                ) || matches!(request.url.scheme(), "ftp" | "ftps" | "sftp")
                    && matches!(
                        name.as_str(),
                        "okytes-user" | "okytes-password" | "okytes-host-key"
                    ))
                    || (!value.is_ascii()
                        && !matches!(request.url.scheme(), "ftp" | "ftps" | "sftp"))
                    || value.bytes().any(|b| b < 32 || b == 127)
            })
        {
            return Err(EngineError::Config("invalid session headers".into()));
        }

        if !self.transport.supports(request.url.scheme())
            || request.url.host_str().is_none()
            || !request.url.username().is_empty()
            || request.url.password().is_some()
        {
            return Err(EngineError::Config(
                "supported URL without embedded credentials required".into(),
            ));
        }
        if request.url.as_str().len() > 8192 {
            return Err(EngineError::Config("URL exceeds 8192 bytes".into()));
        }
        if request
            .expected_hash
            .as_ref()
            .is_some_and(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(EngineError::Config(
                "SHA-256 must be 64 hex characters".into(),
            ));
        }
        tokio::fs::create_dir_all(&request.output_dir)
            .await
            .map_err(|e| io(&request.output_dir, e))?;
        let dir = tokio::fs::canonicalize(&request.output_dir)
            .await
            .map_err(|e| io(&request.output_dir, e))?;
        let name = sanitize(
            request
                .filename
                .as_deref()
                .unwrap_or_else(|| request.url.path().rsplit('/').next().unwrap_or("download")),
        );
        let output_path = resolve_conflict(&dir.join(name), request.conflict_strategy)?;
        let id = capture_id.unwrap_or_else(Uuid::new_v4);
        let requires_session = !request.headers.is_empty();
        if requires_session {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| EngineError::Config("session lock failed".into()))?;
            if sessions.len() >= 128 {
                return Err(EngineError::Config("browser session limit reached".into()));
            }
            sessions.insert(id, request.headers);
        }
        let partial_path = dir.join(format!(".{id}.okytes.part"));
        let job = JobRecord {
            protocol: if matches!(request.url.scheme(), "http" | "https") {
                crate::types::http_protocol()
            } else {
                request.url.scheme().into()
            },
            capture_id,
            requires_session,
            use_server_filename: request.filename.is_none(),
            verified_sha256: None,
            id,
            url: request.url,
            output_path,
            partial_path,
            total_size: None,
            remote_identity: RemoteIdentity::empty(),
            state: JobState::Queued,
            expected_hash: request.expected_hash,
            conflict_strategy: request.conflict_strategy,
            headers: vec![],
            priority: request.priority,
            bytes_downloaded: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            retry_count: 0,
            error_message: None,
        };
        if let Err(error) = self.store.save_job(&job).await {
            self.forget_session(id);
            return Err(error);
        }
        Ok(id)
    }
    /// Last successful response status and time to headers, retained for at most 256 jobs.
    pub fn response_diagnostic(&self, id: JobId) -> Option<(u16, f64)> {
        self.responses.lock().ok()?.get(&id).copied()
    }
    /// Live durable bytes for a non-resumable full stream. This is display data,
    /// never a resume checkpoint, and is discarded when its worker releases ownership.
    pub fn live_full_bytes(&self, id: JobId) -> Option<u64> {
        self.full_progress.lock().ok()?.get(&id).copied()
    }
    /// Sample display-only connection activity and session statistics.
    pub fn telemetry(
        &self,
        id: JobId,
    ) -> (
        Vec<crate::telemetry::LinkSnapshot>,
        crate::telemetry::JobStats,
    ) {
        self.telemetry.snapshot(id)
    }
    /// Live transfer tasks, including admission waits and retry backoff, excluding
    /// probing and verification. Persisted segment states are not worker counts.
    pub fn active_workers(&self, id: JobId) -> u32 {
        self.workers
            .lock()
            .ok()
            .and_then(|counts| counts.get(&id).copied())
            .unwrap_or(0)
    }
    fn worker(&self, id: JobId) -> EngineResult<Worker<'_>> {
        let mut counts = self
            .workers
            .lock()
            .map_err(|e| EngineError::Scheduler(e.to_string()))?;
        *counts.entry(id).or_default() += 1;
        Ok(Worker {
            counts: &self.workers,
            id,
        })
    }
    fn response_received(&self, id: JobId, status: u16, started: Instant) {
        if let Ok(mut responses) = self.responses.lock() {
            if responses.len() >= 256 && !responses.contains_key(&id) {
                if let Some(old) = responses.keys().next().copied() {
                    responses.remove(&old);
                }
            }
            responses.insert(id, (status, started.elapsed().as_secs_f64() * 1000.0));
        }
    }
    async fn transition(&self, id: JobId, state: JobState) -> EngineResult<JobRecord> {
        let old = self.status(id).await?.state;
        let job = self.store.transition_job(id, state).await?;
        let _ = self.events.send(EngineEvent::JobStateChanged {
            job_id: id,
            old_state: old,
            new_state: state,
        });
        Ok(job)
    }
    async fn running(&self, id: JobId) -> EngineResult<bool> {
        Ok(self
            .store
            .load_job(id)
            .await?
            .ok_or(EngineError::JobNotFound(id))?
            .state
            .is_active())
    }
    /// Execute a queued or crash-interrupted job with bounded admission.
    pub async fn execute_job(&self, id: JobId) -> EngineResult<()> {
        if !self.transport.supports(self.status(id).await?.url.scheme())
            || self.status(id).await?.protocol == "torrent"
        {
            return Err(EngineError::Config(
                "job requires a different protocol executor".into(),
            ));
        }
        let _permit = self
            .jobs
            .acquire()
            .await
            .map_err(|e| EngineError::Scheduler(e.to_string()))?;
        {
            let mut active = self
                .active
                .lock()
                .map_err(|e| EngineError::Scheduler(e.to_string()))?;
            if !active.insert(id) {
                return Err(EngineError::Scheduler("job already running".into()));
            }
        }
        let _active = Active {
            ids: &self.active,
            id,
            full_progress: &self.full_progress,
            traffic: &self.traffic,
            telemetry: &self.telemetry,
        };
        let state = self.status(id).await?.state;
        if state == JobState::Cancelled {
            remove_partial(&self.status(id).await?.partial_path).await?;
            return Ok(());
        }
        if state == JobState::Verifying {
            let job = self.status(id).await?;
            if let Some(expected) = &job.verified_sha256 {
                if job.output_path.exists() && hash_file(&job.output_path).await? == *expected {
                    remove_partial(&job.partial_path).await?;
                    self.transition(id, JobState::Completed).await?;
                    return Ok(());
                }
            }
        }
        if state.is_terminal() || state == JobState::Paused {
            return Ok(());
        }
        while !self.queue_allowed()? {
            if matches!(
                self.status(id).await?.state,
                JobState::Paused | JobState::Cancelled
            ) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if state.is_active() || state == JobState::Failed {
            self.transition(id, JobState::Queued).await?;
        }
        let mut result = self.transfer(id).await;
        if matches!(
            self.status(id).await?.state,
            JobState::Paused | JobState::Cancelled
        ) {
            result = Ok(());
        }
        if let Err(ref error) = result {
            self.store
                .record_diagnostic(id, Some(error.to_string()), false)
                .await?;
            if self.status(id).await?.state.is_active() {
                self.transition(id, JobState::Failed).await?;
            }
            let _ = self.events.send(EngineEvent::JobFailed {
                job_id: id,
                error: error.to_string(),
            });
        }
        if self.status(id).await?.state == JobState::Cancelled {
            remove_partial(&self.status(id).await?.partial_path).await?;
        }
        if self.status(id).await?.state.is_terminal() {
            self.forget_session(id);
        }
        result
    }
    async fn probe(&self, job: &JobRecord) -> EngineResult<crate::transport::ProbeResult> {
        let headers = self.session_headers(job)?;
        for attempt in 0..=self.config.max_retries {
            if !self.running(job.id).await? {
                return Err(EngineError::Scheduler("job interrupted".into()));
            }
            let Some(result) = self
                .controlled(job.id, self.transport.probe(&job.url, &headers))
                .await?
            else {
                return Err(EngineError::Scheduler("job interrupted".into()));
            };
            match result {
                Ok(probe) => return Ok(probe),
                Err(error) if retryable(&error) && attempt < self.config.max_retries => {
                    self.delay(job.id, attempt, &error).await?
                }
                Err(source) => {
                    return Err(EngineError::Transport {
                        job_id: job.id,
                        source: Box::new(source),
                    })
                }
            }
        }
        Err(EngineError::Scheduler("retry budget exhausted".into()))
    }
    async fn delay(&self, id: JobId, attempt: u32, error: &TransportError) -> EngineResult<()> {
        self.store
            .record_diagnostic(id, Some(error.to_string()), true)
            .await?;
        let after = match error {
            TransportError::Throttled {
                retry_after_secs, ..
            } => *retry_after_secs,
            _ => None,
        };
        let deadline = tokio::time::Instant::now()
            + retry_delay(
                attempt,
                self.config.retry_base_delay_ms,
                self.config.retry_max_delay_ms,
                after,
            );
        while tokio::time::Instant::now() < deadline {
            if !self.running(id).await? {
                return Err(EngineError::Scheduler("job interrupted".into()));
            }
            tokio::time::sleep_until(
                deadline.min(tokio::time::Instant::now() + Duration::from_millis(100)),
            )
            .await;
        }
        Ok(())
    }
    async fn controlled<T, E>(
        &self,
        id: JobId,
        operation: impl Future<Output = Result<T, E>>,
    ) -> EngineResult<Option<Result<T, E>>> {
        tokio::pin!(operation);
        loop {
            tokio::select! {
                result=&mut operation=>return Ok(Some(result)),
                _=tokio::time::sleep(Duration::from_millis(100))=>{if !self.running(id).await? {return Ok(None);}}
            }
        }
    }
    async fn transfer(&self, id: JobId) -> EngineResult<()> {
        let mut job = self.transition(id, JobState::Probing).await?;
        let probe = self.probe(&job).await?;
        let mut segments = self.store.load_segments(id).await?;
        let durable = segments.iter().map(|s| s.bytes_downloaded).sum::<u64>();
        let resumable_identity = if matches!(job.protocol.as_str(), "ftp" | "ftps" | "sftp") {
            job.remote_identity.last_modified.is_some()
                && job.remote_identity.content_length.is_some()
        } else {
            job.remote_identity.if_range().is_some()
                && job.remote_identity.content_length.is_some()
        };
        if durable > 0
            && (!resumable_identity || !job.remote_identity.is_compatible(&probe.remote_identity))
        {
            return Err(EngineError::IdentityMismatch {
                job_id: id,
                detail: "resume requires the same strong validator, modification time and length"
                    .into(),
            });
        }
        if !self.running(id).await? {
            return Ok(());
        }
        if job.use_server_filename && durable == 0 {
            if let Some(name) = &probe.suggested_filename {
                let parent = job.output_path.parent().ok_or_else(|| {
                    EngineError::Config("destination has no parent directory".into())
                })?;
                job.output_path =
                    resolve_conflict(&parent.join(sanitize(name)), job.conflict_strategy)?;
            }
            job.use_server_filename = false;
        }
        job.remote_identity = probe.remote_identity;
        job.total_size = probe.content_length;
        self.store.save_job(&job).await?;
        job = self.transition(id, JobState::Downloading).await?;
        self.telemetry.started(
            id,
            probe.accepts_ranges && job.total_size.is_some(),
            durable,
        );
        if !probe.accepts_ranges || job.total_size.is_none() {
            if durable > 0 {
                return Err(EngineError::IdentityMismatch {
                    job_id: id,
                    detail: "server no longer permits range resume".into(),
                });
            }
            let mut completed = false;
            for attempt in 0..=self.config.max_retries {
                match self.full(&job).await {
                    Ok(()) => {
                        completed = true;
                        break;
                    }
                    Err(EngineError::Transport { source, .. })
                        if retryable(&source) && attempt < self.config.max_retries =>
                    {
                        self.delay(id, attempt, &source).await?
                    }
                    Err(error) => return Err(error),
                }
            }
            if !completed {
                return Err(EngineError::Scheduler(
                    "full transfer retry budget exhausted".into(),
                ));
            }
        } else {
            let size = job.total_size.unwrap_or(0);
            if segments.is_empty() {
                // A crash may leave preallocated data before ownership metadata
                // commits. With no checkpoints, restart this unique partial.
                remove_partial(&job.partial_path).await?;
                let ranges = if let Some(ref ranges) = probe.checkpoint_ranges {
                    if ranges.len() > 1024 {
                        return Err(EngineError::Config(
                            "source segment count exceeds 1024".into(),
                        ));
                    }
                    ranges.clone()
                } else {
                    allocate_ranges(
                        size,
                        1,
                        self.config.min_segment_size,
                        self.config.max_segment_size,
                    )
                };
                validate_ranges(&ranges, size).map_err(EngineError::Scheduler)?;
                let file = self.files.create(&job.partial_path).await?;
                file.set_len(size)
                    .await
                    .map_err(|e| io(&job.partial_path, e))?;
                file.sync_all()
                    .await
                    .map_err(|e| io(&job.partial_path, e))?;
                segments = ranges
                    .into_iter()
                    .map(|r| SegmentRecord {
                        id: Uuid::new_v4(),
                        job_id: id,
                        start: r.start,
                        end: r.end,
                        bytes_downloaded: 0,
                        state: SegmentState::Pending,
                        last_checkpoint: Utc::now(),
                    })
                    .collect();
                self.store.save_segments(&segments).await?;
            } else {
                let ranges = segments
                    .iter()
                    .map(|s| crate::range::ByteRange {
                        start: s.start,
                        end: s.end,
                    })
                    .collect::<Vec<_>>();
                validate_ranges(&ranges, size).map_err(EngineError::Scheduler)?;
                let metadata = tokio::fs::metadata(&job.partial_path)
                    .await
                    .map_err(|e| io(&job.partial_path, e))?;
                if metadata.len() != size
                    || segments.iter().any(|s| s.bytes_downloaded > s.length())
                {
                    return Err(EngineError::Scheduler(
                        "partial file/checkpoints inconsistent".into(),
                    ));
                }
            }
            let mut total_segments = segments.len();
            // Every split adds an ownership record. Bound them separately from
            // the connection limit, so idle workers can keep taking over the
            // tail of a slow one until the file is done.
            let segment_cap = self.config.max_segments_per_job.saturating_mul(8).min(1024);
            let mut completed_segments = segments.iter().filter(|s| s.is_complete()).count();
            let mut pending: VecDeque<_> =
                segments.into_iter().filter(|s| !s.is_complete()).collect();
            let pulse = Pulse::default();
            let mut flights: HashMap<crate::types::SegmentId, Arc<Flight>> = HashMap::new();
            let mut workers = FuturesUnordered::new();
            let mut tuning =
                crate::scheduler::AdaptiveConcurrency::new(self.config.max_segments_per_job);
            let mut ewma = EwmaThroughput::new(self.config.ewma_alpha);
            let mut progress = durable;
            let mut window = Instant::now();
            let mut first_error = None;
            self.telemetry.observe(
                id,
                self.config.fixed_concurrency.unwrap_or(tuning.limit()),
                0.0,
            );
            while !pending.is_empty() || !workers.is_empty() {
                let admission = self.config.fixed_concurrency.unwrap_or(tuning.limit());
                while !self.queue_allowed()? && self.running(id).await? {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    // A closed schedule window is not a throughput sample.
                    window = Instant::now();
                }
                let live = first_error.is_none() && self.running(id).await?;
                if !live {
                    pending.clear();
                }
                // Give a free worker half of the largest unfinished range,
                // whether it is waiting or being downloaded. Both ownership
                // records are persisted atomically before the new half starts.
                while live
                    && probe.checkpoint_ranges.is_none()
                    && workers.len() + pending.len() < admission
                    && total_segments < segment_cap
                {
                    let idle = pending
                        .iter()
                        .enumerate()
                        .map(|(index, s)| (s.length() - s.bytes_downloaded, index))
                        .max();
                    let busy = flights
                        .iter()
                        .map(|(segment, flight)| (flight.lease.remaining(), *segment))
                        .max();
                    let tail = match (idle, busy) {
                        (Some((left, index)), busy)
                            if busy.is_none_or(|(taken, _)| left >= taken) =>
                        {
                            let Some(mut original) = pending.remove(index) else {
                                break;
                            };
                            let Some((left, right)) = crate::range::split_segment(
                                original.current_offset(),
                                original.end,
                                self.config.min_segment_size,
                            ) else {
                                pending.push_back(original);
                                break;
                            };
                            original.end = left.end;
                            pending.push_back(original.clone());
                            (original, right)
                        }
                        (_, Some((_, victim))) => {
                            let Some(flight) = flights.get(&victim) else {
                                break;
                            };
                            // Its worker is committing a checkpoint; the next
                            // tick sees the settled record.
                            let Ok(mut record) = flight.durable.try_lock() else {
                                break;
                            };
                            // The request stops at the new boundary by itself:
                            // it reserves every byte through the same lease.
                            let Some(right) = flight.lease.steal(self.config.min_segment_size)
                            else {
                                break;
                            };
                            record.end = right.start - 1;
                            (record.clone(), right)
                        }
                        _ => break,
                    };
                    let (original, right) = tail;
                    let stolen = SegmentRecord {
                        id: Uuid::new_v4(),
                        job_id: id,
                        start: right.start,
                        end: right.end,
                        bytes_downloaded: 0,
                        state: SegmentState::Pending,
                        last_checkpoint: Utc::now(),
                    };
                    self.store
                        .save_segments(&[original, stolen.clone()])
                        .await?;
                    pending.push_back(stolen);
                    total_segments += 1;
                }
                while workers.len() < admission && !pending.is_empty() {
                    if let Some(segment) = pending.pop_front() {
                        let stop = segment.end.min(
                            segment
                                .current_offset()
                                .saturating_add(self.config.max_segment_size.max(1) - 1),
                        ) + 1;
                        let flight = Arc::new(Flight {
                            lease: Lease::new(segment.current_offset(), stop, segment.end),
                            durable: tokio::sync::Mutex::new(segment.clone()),
                        });
                        flights.insert(segment.id, flight.clone());
                        workers.push(self.quantum(&job, segment, flight, &pulse));
                    }
                }
                // Wake on a finished request or on the clock, so admission,
                // splitting and throughput sampling do not wait for a large
                // request to end.
                tokio::select! {
                    Some(result) = workers.next() => match result {
                        Ok((mut segment, latency_ms)) => {
                            if let Some(flight) = flights.remove(&segment.id) {
                                segment.end = flight.lease.end();
                            }
                            tuning.health(0, latency_ms);
                            if segment.is_complete() {
                                completed_segments += 1;
                            } else if first_error.is_none() && self.running(id).await? {
                                pending.push_back(segment);
                            }
                        }
                        Err(error) => {
                            first_error.get_or_insert(error);
                        }
                    },
                    _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                }
                let failed = pulse.failures.swap(0, Ordering::Relaxed);
                if failed > 0 {
                    tuning.health(failed, 0.0);
                }
                if window.elapsed() >= Duration::from_millis(250) {
                    progress += pulse.bytes.swap(0, Ordering::Relaxed);
                    ewma.record(pulse.received.swap(0, Ordering::Relaxed), window.elapsed());
                    tuning.observe(ewma.value());
                    self.telemetry.observe(id, admission, ewma.value());
                    window = Instant::now();
                    let _ = self.events.send(EngineEvent::Progress(DownloadProgress {
                        job_id: id,
                        state: JobState::Downloading,
                        bytes_downloaded: progress,
                        total_size: job.total_size,
                        throughput_bps: ewma.value(),
                        active_segments: workers.len(),
                        completed_segments,
                        total_segments,
                    }));
                }
            }
            if let Some(error) = first_error {
                return Err(error);
            }
        }
        if !self.running(id).await? {
            return Ok(());
        }
        self.finalize(id).await
    }
    // One bounded request per call; ownership returns to the scheduler when it
    // ends. Meanwhile a free worker can take the unreserved tail through the
    // flight's lease, without racing this writer.
    async fn quantum(
        &self,
        job: &JobRecord,
        mut segment: SegmentRecord,
        flight: Arc<Flight>,
        pulse: &Pulse,
    ) -> EngineResult<(SegmentRecord, f64)> {
        let _worker = self.worker(job.id)?;
        let headers = self.session_headers(job)?;
        let link = self.telemetry.open(
            job.id,
            segment.start,
            segment.current_offset(),
            Some(flight.lease.stop().saturating_sub(1)),
        );
        // Consecutive failed requests that committed too little to count.
        let mut failures = 0;
        let mut latency_ms = 0.0;
        loop {
            // Bytes a failed request reserved but never committed are free again.
            flight.lease.rewind(segment.current_offset());
            let stop = flight.lease.stop();
            if segment.current_offset() >= stop || !self.running(job.id).await? {
                break;
            }
            if failures > 0 {
                link.set_state(crate::telemetry::LinkState::Connecting);
            }
            let Some(permit) = self
                .controlled(
                    job.id,
                    self.fair
                        .acquire(job.id, job.url.origin().ascii_serialization()),
                )
                .await?
            else {
                break;
            };
            let permit = permit?;
            let response_started = Instant::now();
            let Some(response) = self
                .controlled(
                    job.id,
                    self.transport.fetch(
                        &job.url,
                        &headers,
                        Some((segment.current_offset(), stop - 1)),
                        &job.remote_identity,
                    ),
                )
                .await?
            else {
                break;
            };
            latency_ms = response_started.elapsed().as_secs_f64() * 1000.0;
            let before = segment.bytes_downloaded;
            let result = match response {
                Ok(response) => {
                    self.response_received(job.id, response.status, response_started);
                    if segment.state == SegmentState::Failed {
                        segment.state = segment.state.transition(SegmentState::Pending, job.id)?;
                    }
                    segment.state = segment.state.transition(SegmentState::Active, job.id)?;
                    link.set_state(crate::telemetry::LinkState::Downloading);
                    self.stream(
                        job,
                        &mut segment,
                        response.body,
                        Some(stop),
                        Some((&*flight, pulse)),
                        &link,
                    )
                    .await
                }
                Err(source) => Err(EngineError::Transport {
                    job_id: job.id,
                    source: Box::new(source),
                }),
            };
            drop(permit);
            match result {
                Ok(()) => break,
                Err(EngineError::Transport { source, .. }) if retryable(&source) => {
                    if segment.bytes_downloaded - before >= RETRY_PROGRESS_BYTES {
                        failures = 0;
                    }
                    if failures >= self.config.max_retries {
                        return Err(EngineError::Transport {
                            job_id: job.id,
                            source,
                        });
                    }
                    pulse.failures.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(job_id=%job.id,attempt=failures,"retrying segment");
                    link.retrying(failures + 1);
                    self.delay(job.id, failures, &source).await?;
                    failures += 1;
                }
                Err(error) => return Err(error),
            }
        }
        // A split may have moved the end while this request ran.
        let mut durable = flight.durable.lock().await;
        segment.end = flight.lease.end();
        if segment.is_complete() {
            segment.state = segment.state.transition(SegmentState::Complete, job.id)?;
            self.store.checkpoint_segment(&segment).await?;
            *durable = segment.clone();
        }
        drop(durable);
        Ok((segment, latency_ms))
    }
    // `shared` is present for a resumable ranged request: bytes are reserved
    // through its lease and checkpointed. A full stream passes `None`.
    async fn stream(
        &self,
        job: &JobRecord,
        segment: &mut SegmentRecord,
        mut body: crate::transport::ByteStream,
        end: Option<u64>,
        shared: Option<(&Flight, &Pulse)>,
        link: &crate::telemetry::LinkGuard<'_>,
    ) -> EngineResult<()> {
        let buffer_limit = self.config.write_buffer_size.min(256 * 1024);
        let mut pending = Vec::with_capacity(buffer_limit);
        let mut control_check = Instant::now() - Duration::from_millis(100);
        // Only a resumable request is worth replacing when it goes silent.
        let stall =
            shared.map(|_| Duration::from_secs(self.config.read_timeout_secs.min(STALL_SECS)));
        let mut last_data = Instant::now();
        let mut failure = None;
        let mut finished = false;
        'body: loop {
            if control_check.elapsed() >= Duration::from_millis(100) {
                if !self.running(job.id).await? {
                    break;
                }
                control_check = Instant::now();
            }
            let next = tokio::select! {
                next = body.next() => next,
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if stall.is_some_and(|limit| last_data.elapsed() >= limit) {
                        failure = Some(TransportError::Timeout {
                            url: Box::new(job.url.clone()),
                        });
                        break;
                    }
                    continue;
                }
            };
            let Some(chunk) = next else {
                finished = true;
                break;
            };
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(source) => {
                    failure = Some(source);
                    break;
                }
            };
            if end.is_some_and(|end| {
                segment
                    .current_offset()
                    .saturating_add(pending.len() as u64)
                    .saturating_add(chunk.len() as u64)
                    > end
            }) {
                return Err(EngineError::Scheduler("body exceeded owned range".into()));
            }
            let mut incoming = chunk.as_ref();
            // Another worker may have taken the tail of this range: accept
            // only the bytes still owned, then end the request.
            let mut bounded = false;
            if let Some((flight, _)) = shared {
                let granted = flight.lease.reserve(incoming.len() as u64) as usize;
                bounded = granted < incoming.len();
                incoming = &incoming[..granted];
            }
            while !incoming.is_empty() {
                let wanted = incoming.len().min(buffer_limit - pending.len()).min(16384);
                let take = loop {
                    let granted = self.traffic.take(
                        job.id,
                        wanted,
                        tokio::time::Instant::now(),
                        Utc::now(),
                    )?;
                    if granted > 0 {
                        break granted;
                    }
                    if !self.running(job.id).await? {
                        break 'body;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                };
                pending.extend_from_slice(&incoming[..take]);
                link.received(take);
                if let Some((_, pulse)) = shared {
                    pulse.received.fetch_add(take as u64, Ordering::Relaxed);
                }
                incoming = &incoming[take..];
                if pending.len() == buffer_limit {
                    self.durable_checkpoint(&job.partial_path, segment, &pending, shared)
                        .await?;
                    pending.clear();
                }
            }
            last_data = Instant::now();
            if bounded {
                break;
            }
        }
        // Keep what arrived before a pause, a split or a dropped connection:
        // the next request continues from here instead of fetching it again.
        if !pending.is_empty() {
            self.durable_checkpoint(&job.partial_path, segment, &pending, shared)
                .await?;
        }
        if let Some(source) = failure {
            return Err(EngineError::Transport {
                job_id: job.id,
                source: Box::new(source),
            });
        }
        if finished && end.is_some_and(|end| segment.current_offset() != end) {
            return Err(EngineError::Transport {
                job_id: job.id,
                source: Box::new(TransportError::DataCorruption {
                    url: Box::new(job.url.clone()),
                }),
            });
        }
        Ok(())
    }
    /// Single ordering point: write → sync_data → checkpoint commit.
    async fn durable_checkpoint(
        &self,
        path: &Path,
        segment: &mut SegmentRecord,
        bytes: &[u8],
        shared: Option<(&Flight, &Pulse)>,
    ) -> EngineResult<()> {
        let offset = segment.current_offset();
        let mut next = segment.clone();
        next.bytes_downloaded += bytes.len() as u64;
        next.last_checkpoint = Utc::now();
        let committed = &mut next;
        commit_durable(
            async {
                let mut file = self.files.open(path).await?;
                file.seek(std::io::SeekFrom::Start(offset))
                    .await
                    .map_err(|e| io(path, e))?;
                file.write_all(bytes).await.map_err(|e| io(path, e))?;
                Ok(file)
            },
            |file: tokio::fs::File| async move { file.sync_data().await.map_err(|e| io(path, e)) },
            async {
                if let Some((flight, _)) = shared {
                    // Read the end under the record lock: a split persisted
                    // since the last checkpoint must not be written back.
                    let mut durable = flight.durable.lock().await;
                    committed.end = flight.lease.end();
                    self.store.checkpoint_segment(committed).await?;
                    *durable = committed.clone();
                }
                Ok(())
            },
        )
        .await?;
        match shared {
            Some((_, pulse)) => {
                pulse.bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
            }
            None => {
                if let Ok(mut progress) = self.full_progress.lock() {
                    progress.insert(next.job_id, next.bytes_downloaded);
                }
            }
        }
        *segment = next;
        Ok(())
    }
    async fn full(&self, job: &JobRecord) -> EngineResult<()> {
        let headers = self.session_headers(job)?;
        let _worker = self.worker(job.id)?;
        if let Ok(mut progress) = self.full_progress.lock() {
            progress.insert(job.id, 0);
        }
        remove_partial(&job.partial_path).await?;
        drop(self.files.create(&job.partial_path).await?);
        let link = self.telemetry.open(
            job.id,
            0,
            0,
            job.total_size.map(|size| size.saturating_sub(1)),
        );
        let response_started = Instant::now();
        let Some(response) = self
            .controlled(
                job.id,
                self.transport
                    .fetch(&job.url, &headers, None, &job.remote_identity),
            )
            .await?
        else {
            return Ok(());
        };
        let response = response.map_err(|source| EngineError::Transport {
            job_id: job.id,
            source: Box::new(source),
        })?;
        self.response_received(job.id, response.status, response_started);
        link.set_state(crate::telemetry::LinkState::Downloading);
        let mut segment = SegmentRecord {
            id: Uuid::new_v4(),
            job_id: job.id,
            start: 0,
            end: job.total_size.unwrap_or(u64::MAX).saturating_sub(1),
            bytes_downloaded: 0,
            state: SegmentState::Active,
            last_checkpoint: Utc::now(),
        };
        self.stream(
            job,
            &mut segment,
            response.body,
            job.total_size,
            None,
            &link,
        )
        .await
    }
    async fn finalize(&self, id: JobId) -> EngineResult<()> {
        let mut job = self.transition(id, JobState::Verifying).await?;
        let file = self.files.open(&job.partial_path).await?;
        let Some(actual) = hash_open_file(file, &job.partial_path, || self.running(id)).await?
        else {
            return Ok(());
        };
        if let Some(expected) = &job.expected_hash {
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(EngineError::IntegrityMismatch {
                    job_id: id,
                    expected: expected.clone(),
                    actual,
                });
            }
        }
        if !self.running(id).await? {
            return Ok(());
        }
        job.total_size = Some(
            tokio::fs::metadata(&job.partial_path)
                .await
                .map_err(|e| io(&job.partial_path, e))?
                .len(),
        );
        job.bytes_downloaded = job.total_size.unwrap_or(0);
        job.output_path = resolve_conflict(&job.output_path, job.conflict_strategy)?;
        job.verified_sha256 = Some(actual.clone());
        self.store.save_job(&job).await?;
        match job.conflict_strategy {
            ConflictStrategy::Overwrite => tokio::fs::rename(&job.partial_path, &job.output_path)
                .await
                .map_err(|e| io(&job.output_path, e))?,
            _ => {
                tokio::fs::hard_link(&job.partial_path, &job.output_path)
                    .await
                    .map_err(|e| io(&job.output_path, e))?;
                remove_partial(&job.partial_path).await?;
            }
        }
        self.transition(id, JobState::Completed).await?;
        let _ = self.events.send(EngineEvent::JobCompleted { job_id: id });
        tracing::info!(job_id=%id,sha256=%actual,"download verified and published");
        Ok(())
    }
}
fn retryable(error: &TransportError) -> bool {
    matches!(error,TransportError::Request {detail,..} if !detail.contains("identity") && !detail.contains("validator"))
        || matches!(
            error,
            TransportError::Throttled { .. }
                | TransportError::Timeout { .. }
                | TransportError::Io { .. }
                | TransportError::DataCorruption { .. }
        )
}

// Keep the durability barrier and commit in one independently fault-testable
// sequencing primitive. Commit is never polled if sync fails or is cancelled.
async fn commit_durable<T, S: Future<Output = EngineResult<()>>>(
    write: impl Future<Output = EngineResult<T>>,
    sync: impl FnOnce(T) -> S,
    commit: impl Future<Output = EngineResult<()>>,
) -> EngineResult<()> {
    let data = write.await?;
    sync(data).await?;
    commit.await
}

async fn remove_partial(path: &Path) -> EngineResult<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io(path, e)),
    }
}
/// Compute SHA-256 using a fixed 64 KiB buffer.
pub async fn hash_file(path: &Path) -> EngineResult<String> {
    let file = tokio::fs::File::open(path).await.map_err(|e| io(path, e))?;
    hash_open_file(file, path, || async { Ok(true) })
        .await?
        .ok_or_else(|| EngineError::Scheduler("hash verification interrupted".into()))
}
async fn hash_open_file<F: FnMut() -> R, R: Future<Output = EngineResult<bool>>>(
    mut file: tokio::fs::File,
    path: &Path,
    mut running: F,
) -> EngineResult<Option<String>> {
    let mut buffer = vec![0; 64 * 1024];
    let mut hash = Sha256::new();
    let mut checked = Instant::now() - Duration::from_millis(100);
    loop {
        if checked.elapsed() >= Duration::from_millis(100) {
            if !running().await? {
                return Ok(None);
            }
            checked = Instant::now();
        }
        let n = file.read(&mut buffer).await.map_err(|e| io(path, e))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(Some(format!("{:x}", hash.finalize())))
}
/// Sanitize a suggested filename across supported platform naming rules.
pub fn sanitize(name: &str) -> String {
    let name: String = name
        .chars()
        .take(180)
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut name = name.trim_matches(['.', ' ']).to_owned();
    if name.is_empty() {
        name = "download".into();
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str())
        || (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
    {
        name.insert(0, '_');
    }
    name
}
fn resolve_conflict(path: &Path, strategy: ConflictStrategy) -> EngineResult<PathBuf> {
    if !path.exists() || strategy == ConflictStrategy::Overwrite {
        return Ok(path.into());
    }
    if strategy == ConflictStrategy::Fail {
        return Err(EngineError::Config("destination exists".into()));
    }
    for n in 1..=9999 {
        let name = format!(
            "{} ({n}){}",
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("download"),
            path.extension()
                .and_then(|s| s.to_str())
                .map(|s| format!(".{s}"))
                .unwrap_or_default()
        );
        let candidate = path.with_file_name(name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(EngineError::Config("too many conflicting filenames".into()))
}
impl DownloadService for Engine {
    fn add(&self, request: DownloadRequest) -> ServiceFuture<'_, JobId> {
        Box::pin(self.add_request(request, None))
    }
    fn pause(&self, id: JobId) -> ServiceFuture<'_, ()> {
        Box::pin(async move {
            self.transition(id, JobState::Paused).await?;
            Ok(())
        })
    }
    fn resume(&self, id: JobId) -> ServiceFuture<'_, ()> {
        Box::pin(async move {
            let state = self.status(id).await?.state;
            if state == JobState::Queued {
                return Ok(());
            }
            if !state.is_resumable() {
                return Err(EngineError::InvalidTransition {
                    job_id: id,
                    detail: "only paused or failed jobs can be resumed".into(),
                });
            }
            self.transition(id, JobState::Queued).await?;
            Ok(())
        })
    }
    fn cancel(&self, id: JobId) -> ServiceFuture<'_, ()> {
        Box::pin(async move {
            let job = self.transition(id, JobState::Cancelled).await?;
            let _ = job; // Runner ownership is required for cleanup after workers stop.
            Ok(())
        })
    }
    fn list(&self, filter: Option<JobState>) -> ServiceFuture<'_, Vec<JobRecord>> {
        Box::pin(self.store.list_jobs(filter))
    }
    fn status(&self, id: JobId) -> ServiceFuture<'_, JobRecord> {
        Box::pin(async move {
            let mut job = self
                .store
                .load_job(id)
                .await?
                .ok_or(EngineError::JobNotFound(id))?;
            if job.protocol != "torrent" && job.state != JobState::Completed {
                job.bytes_downloaded = self
                    .store
                    .load_segments(id)
                    .await?
                    .iter()
                    .map(|s| s.bytes_downloaded)
                    .sum();
            }
            Ok(job)
        })
    }
    fn subscribe(&self) -> ServiceFuture<'_, broadcast::Receiver<EngineEvent>> {
        Box::pin(async move { Ok(self.events.subscribe()) })
    }
}

#[cfg(test)]
mod durability_tests {
    use super::*;
    #[tokio::test]
    async fn checkpoint_is_not_polled_before_sync_or_after_sync_failure() {
        let committed = std::sync::atomic::AtomicBool::new(false);
        let result = commit_durable(
            async { Ok(()) },
            |()| async { Err(EngineError::Scheduler("injected sync failure".into())) },
            async {
                committed.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        )
        .await;
        assert!(result.is_err());
        assert!(!committed.load(std::sync::atomic::Ordering::SeqCst));
        let synced = std::sync::atomic::AtomicBool::new(false);
        let written = std::sync::atomic::AtomicBool::new(false);
        commit_durable(
            async {
                written.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
            |()| async {
                assert!(written.load(std::sync::atomic::Ordering::SeqCst));
                tokio::task::yield_now().await;
                synced.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
            async {
                assert!(synced.load(std::sync::atomic::Ordering::SeqCst));
                Ok(())
            },
        )
        .await
        .unwrap();
        let result = commit_durable(
            async { Err::<(), _>(EngineError::Scheduler("injected write failure".into())) },
            |()| async { panic!("sync must not run after failed write") },
            async { panic!("commit must not run after failed write") },
        )
        .await;
        assert!(result.is_err());
    }
}
