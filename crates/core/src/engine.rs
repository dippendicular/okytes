//! Transfer orchestration. Workers own disjoint ranges and yield at bounded
//! request boundaries. Data is synced before checkpoint commits. Control state
//! is authoritative in SQLite, allowing another CLI to pause a foreground runner.
pub use crate::store::JobStore;
use crate::{
    error::{EngineError, EngineResult, TransportError},
    range::{allocate_ranges, validate_ranges},
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
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{broadcast, Semaphore},
};
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
}
struct Active<'a> {
    ids: &'a Mutex<HashSet<JobId>>,
    id: JobId,
    full_progress: &'a Mutex<HashMap<JobId, u64>>,
}
struct Worker<'a> {
    counts: &'a Mutex<HashMap<JobId, u32>>,
    id: JobId,
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
        })
    }
    /// Add a browser job using its request UUID for durable idempotency. Only the
    /// authenticated queue owner may call this after checking existing records.
    pub async fn add_captured(&self, request: DownloadRequest, id: JobId) -> EngineResult<JobId> {
        self.add_request(request, Some(id)).await
    }
    /// Drop memory-only credentials after cancellation, removal or completion.
    pub fn forget_session(&self, id: JobId) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(&id);
        }
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
                    "Browser session expired; capture this download again from the browser".into(),
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
                !matches!(
                    name.to_ascii_lowercase().as_str(),
                    "cookie" | "authorization" | "referer" | "user-agent" | "accept"
                ) || !value.is_ascii()
                    || value.bytes().any(|b| b < 32 || b == 127)
            })
        {
            return Err(EngineError::Config("invalid session headers".into()));
        }

        if !matches!(request.url.scheme(), "http" | "https")
            || !request.url.username().is_empty()
            || request.url.password().is_some()
        {
            return Err(EngineError::Config(
                "HTTP(S) URL without embedded credentials required".into(),
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
    async fn controlled<T>(
        &self,
        id: JobId,
        operation: impl Future<Output = Result<T, TransportError>>,
    ) -> EngineResult<Option<Result<T, TransportError>>> {
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
        if durable > 0
            && (job.remote_identity.strong_etag().is_none()
                || !job.remote_identity.is_compatible(&probe.remote_identity))
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
                let ranges = allocate_ranges(
                    size,
                    1,
                    self.config.min_segment_size,
                    self.config.max_segment_size,
                );
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
            let mut completed_segments = segments.iter().filter(|s| s.is_complete()).count();
            let mut pending: VecDeque<_> =
                segments.into_iter().filter(|s| !s.is_complete()).collect();
            let mut workers = FuturesUnordered::new();
            let mut tuning =
                crate::scheduler::AdaptiveConcurrency::new(self.config.max_segments_per_job);
            let mut ewma = EwmaThroughput::new(self.config.ewma_alpha);
            let mut progress = durable;
            let mut sampled = 0;
            let mut window = Instant::now();
            let mut first_error = None;
            while !pending.is_empty() || !workers.is_empty() {
                if first_error.is_some() || !self.running(id).await? {
                    pending.clear();
                }
                // Split only relinquished tails: no active response can still
                // own either half. Persist both ownership updates atomically.
                while workers.len() + pending.len() < tuning.limit()
                    && total_segments < self.config.max_segments_per_job
                {
                    let Some(index) = pending
                        .iter()
                        .enumerate()
                        .max_by_key(|(_, s)| s.length() - s.bytes_downloaded)
                        .map(|(i, _)| i)
                    else {
                        break;
                    };
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
                        .save_segments(&[original.clone(), stolen.clone()])
                        .await?;
                    pending.push_back(original);
                    pending.push_back(stolen);
                    total_segments += 1;
                }
                while workers.len() < tuning.limit() && !pending.is_empty() {
                    if let Some(segment) = pending.pop_front() {
                        workers.push(self.quantum(&job, segment));
                    }
                }
                if let Some(result) = workers.next().await {
                    let (segment, bytes) = match result {
                        Ok(result) => result,
                        Err(error) => {
                            first_error.get_or_insert(error);
                            continue;
                        }
                    };
                    progress += bytes;
                    sampled += bytes;
                    if segment.is_complete() {
                        completed_segments += 1;
                    }
                    if first_error.is_none() && !segment.is_complete() && self.running(id).await? {
                        pending.push_back(segment);
                    }
                    if window.elapsed() >= Duration::from_millis(100) {
                        ewma.record(sampled, window.elapsed());
                        tuning.observe(ewma.value());
                        sampled = 0;
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
    // Each bounded request relinquishes range ownership. Idle workers can take
    // subsequent quanta of a slow segment without racing an active writer.
    async fn quantum(
        &self,
        job: &JobRecord,
        mut segment: SegmentRecord,
    ) -> EngineResult<(SegmentRecord, u64)> {
        let _worker = self.worker(job.id)?;
        let headers = self.session_headers(job)?;
        let before = segment.bytes_downloaded;
        let end = segment.end.min(
            segment
                .current_offset()
                .saturating_add(self.config.max_segment_size.max(1) - 1),
        );
        for attempt in 0..=self.config.max_retries {
            if !self.running(job.id).await? {
                return Ok((segment, 0));
            }
            let response_started = Instant::now();
            let Some(response) = self
                .controlled(
                    job.id,
                    self.transport.fetch(
                        &job.url,
                        &headers,
                        Some((segment.current_offset(), end)),
                        &job.remote_identity,
                    ),
                )
                .await?
            else {
                let delta = segment.bytes_downloaded - before;
                return Ok((segment, delta));
            };
            let result = match response {
                Ok(response) => {
                    self.response_received(job.id, response.status, response_started);
                    if segment.state == SegmentState::Failed {
                        segment.state = segment.state.transition(SegmentState::Pending, job.id)?;
                    }
                    segment.state = segment.state.transition(SegmentState::Active, job.id)?;
                    self.stream(job, &mut segment, response.body, Some(end + 1), true)
                        .await
                }
                Err(source) => Err(EngineError::Transport {
                    job_id: job.id,
                    source: Box::new(source),
                }),
            };
            match result {
                Ok(()) => {
                    if segment.is_complete() {
                        segment.state = segment.state.transition(SegmentState::Complete, job.id)?;
                        self.store.checkpoint_segment(&segment).await?;
                    }
                    let delta = segment.bytes_downloaded - before;
                    return Ok((segment, delta));
                }
                Err(EngineError::Transport { source, .. })
                    if retryable(&source) && attempt < self.config.max_retries =>
                {
                    tracing::warn!(job_id=%job.id,attempt,"retrying segment");
                    self.delay(job.id, attempt, &source).await?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(EngineError::Scheduler("retry budget exhausted".into()))
    }
    async fn stream(
        &self,
        job: &JobRecord,
        segment: &mut SegmentRecord,
        mut body: crate::transport::ByteStream,
        end: Option<u64>,
        checkpoint: bool,
    ) -> EngineResult<()> {
        let buffer_limit = self.config.write_buffer_size.min(256 * 1024);
        let mut pending = Vec::with_capacity(buffer_limit);
        let mut control_check = Instant::now() - Duration::from_millis(100);
        loop {
            if control_check.elapsed() >= Duration::from_millis(100) {
                if !self.running(job.id).await? {
                    return Ok(());
                }
                control_check = Instant::now();
            }
            let next = tokio::select! {next=body.next()=>next,_=tokio::time::sleep(Duration::from_millis(100))=>continue};
            let Some(chunk) = next else { break };
            let chunk = chunk.map_err(|source| EngineError::Transport {
                job_id: job.id,
                source: Box::new(source),
            })?;
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
            while !incoming.is_empty() {
                let take = incoming.len().min(buffer_limit - pending.len());
                pending.extend_from_slice(&incoming[..take]);
                incoming = &incoming[take..];
                if pending.len() == buffer_limit {
                    self.durable_checkpoint(&job.partial_path, segment, &pending, checkpoint)
                        .await?;
                    pending.clear();
                }
            }
        }
        if !pending.is_empty() {
            self.durable_checkpoint(&job.partial_path, segment, &pending, checkpoint)
                .await?;
        }
        if end.is_some_and(|end| segment.current_offset() != end) {
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
        persist: bool,
    ) -> EngineResult<()> {
        let offset = segment.current_offset();
        let mut next = segment.clone();
        next.bytes_downloaded += bytes.len() as u64;
        next.last_checkpoint = Utc::now();
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
                if persist {
                    self.store.checkpoint_segment(&next).await?;
                }
                Ok(())
            },
        )
        .await?;
        if !persist {
            if let Ok(mut progress) = self.full_progress.lock() {
                progress.insert(next.job_id, next.bytes_downloaded);
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
        let mut segment = SegmentRecord {
            id: Uuid::new_v4(),
            job_id: job.id,
            start: 0,
            end: job.total_size.unwrap_or(u64::MAX).saturating_sub(1),
            bytes_downloaded: 0,
            state: SegmentState::Active,
            last_checkpoint: Utc::now(),
        };
        self.stream(job, &mut segment, response.body, job.total_size, false)
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
            if job.state != JobState::Completed {
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
