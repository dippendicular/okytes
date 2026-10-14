//! Embedded queue owner. Platform adapters contain no scheduling or persistence rules.
//! A single admission mutex prevents controls racing worker startup. Controls wait
//! for worker quiescence before mutation; snapshots never report speculative bytes.
use okytes_core::store::JobStore;
use okytes_core::{
    desktop::{Action, AddInput, DesktopService, Job, Reply, Segment, Settings, Snapshot},
    engine::Engine,
    error::{EngineError, EngineResult},
    service::{DownloadService, EngineEvent},
    state::JobState,
    types::{ConflictStrategy, DownloadRequest, JobId, JobRecord},
};
use okytes_persistence::{files::LocalFiles, EngineLease, SqliteStore};
use okytes_transport_http::HttpTransport;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Weak},
    time::{Duration, Instant},
};
use tokio::{sync::Mutex, task::JoinHandle};
use uuid::Uuid;

struct Queue {
    stopping: bool,
    tasks: HashMap<JobId, JoinHandle<()>>,
    settings: Settings,
    samples: HashMap<JobId, (Instant, u64, f64)>,
    revision: u64,
}

/// Owns the process lease, engine and bounded worker set for the app lifetime.
pub struct Session {
    database_path: PathBuf,
    engine: Arc<Engine>,
    store: Arc<SqliteStore>,
    queue: Mutex<Queue>,
    _lease: Arc<EngineLease>,
}

impl Drop for Session {
    fn drop(&mut self) {
        for (_, task) in self.queue.get_mut().tasks.drain() {
            task.abort();
        }
    }
}

fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::Config(message.into())
}
fn id(value: &str) -> EngineResult<JobId> {
    Uuid::parse_str(value).map_err(|_| invalid("invalid job ID"))
}

impl Session {
    /// Full engine metadata for CLI compatibility; session headers are omitted.
    pub async fn engine_record(&self, id: JobId) -> EngineResult<JobRecord> {
        self.engine.status(id).await
    }
    /// Bounded legacy CLI history page.
    pub async fn engine_page(&self, offset: usize) -> EngineResult<Vec<JobRecord>> {
        self.store.page(offset).await
    }
    /// Persisted engine configuration; transport changes apply on next startup.
    pub async fn configuration(&self) -> EngineResult<okytes_core::types::EngineConfig> {
        let queue = self.queue.lock().await;
        let mut config = self.store.config().await?;
        config.database_path = self.database_path.clone();
        config.max_concurrent_jobs = queue.settings.concurrency as usize;
        config.default_output_dir = PathBuf::from(&queue.settings.destination);
        Ok(config)
    }
    /// Validate configuration without allowing it to redirect this profile's DB.
    pub async fn set_configuration(
        &self,
        mut config: okytes_core::types::EngineConfig,
    ) -> EngineResult<()> {
        config.validate()?;
        let mut queue = self.queue.lock().await;
        let mut settings = queue.settings.clone();
        settings.concurrency = config.max_concurrent_jobs as u32;
        settings.destination = config.default_output_dir.to_string_lossy().into_owned();
        validate_settings(&settings)?;
        std::fs::create_dir_all(&config.default_output_dir)
            .map_err(|_| invalid("cannot create destination"))?;
        config.database_path = self.database_path.clone();
        self.store
            .set_preferences(vec![
                (
                    "engine".into(),
                    serde_json::to_string(&config).map_err(|_| invalid("invalid configuration"))?,
                ),
                (
                    "desktop-v1".into(),
                    serde_json::to_string(&settings).map_err(|_| invalid("invalid settings"))?,
                ),
            ])
            .await?;
        queue.settings = settings;
        Ok(())
    }
    /// Stop admission, drain durable writes and requeue interrupted jobs. Jobs
    /// explicitly paused by the user remain paused across daemon shutdown.
    pub async fn shutdown(&self) -> EngineResult<()> {
        let mut queue = self.queue.lock().await;
        queue.stopping = true;
        let ids: Vec<_> = queue.tasks.keys().copied().collect();
        let mut resume = Vec::new();
        for id in &ids {
            let job = self.engine.status(*id).await?;
            if (job.state.is_active() || job.state == JobState::Queued)
                && self.engine.pause(*id).await.is_ok()
            {
                resume.push(*id);
            }
        }
        for id in ids {
            if let Some(task) = queue.tasks.remove(&id) {
                task.await.map_err(|_| invalid("worker shutdown failed"))?;
            }
        }
        for id in resume {
            if self.engine.status(id).await?.state == JobState::Paused {
                self.engine.resume(id).await?;
            }
        }
        Ok(())
    }
    /// Accept one validated browser capture under the same admission lock as UI
    /// controls. Its request UUID is the durable job UUID, so reconnect retries
    /// cannot create duplicate jobs, including after a desktop restart.
    pub async fn capture(
        &self,
        request_id: String,
        capture: okytes_browser_bridge::protocol::Capture,
    ) -> EngineResult<String> {
        let request = okytes_browser_bridge::protocol::Request {
            version: 1,
            id: request_id.clone(),
            operation: okytes_browser_bridge::protocol::Operation::Capture(capture.clone()),
        };
        request.validate().map_err(invalid)?;
        let id = id(&request_id)?;
        let mut queue = self.queue.lock().await;
        if let Some(job) = self.store.load_job(id).await? {
            if job.capture_id == Some(id) {
                return Ok(id.to_string());
            }
            return Err(invalid("request ID collision"));
        }
        let result = self
            .engine
            .add_captured(
                DownloadRequest {
                    url: url::Url::parse(&capture.url).map_err(|_| invalid("invalid source"))?,
                    output_dir: PathBuf::from(&queue.settings.destination),
                    filename: capture.filename,
                    expected_hash: capture.sha256,
                    conflict_strategy: ConflictStrategy::Rename,
                    headers: capture.headers,
                    priority: 0,
                },
                id,
            )
            .await?;
        queue.revision += 1;
        Ok(result.to_string())
    }
    /// Open durable history and recover interrupted transfers with exclusive ownership.
    pub async fn open(database: &Path, destination: &Path) -> EngineResult<Arc<Self>> {
        let store = Arc::new(SqliteStore::open(database)?);
        let lease = SqliteStore::acquire_engine(database)?;
        let settings = match store.preference("desktop-v1").await? {
            Some(value) => serde_json::from_str(&value).map_err(|e| invalid(e.to_string()))?,
            None => Settings {
                version: 1,
                destination: destination.to_string_lossy().into(),
                concurrency: 4,
                theme: "system".into(),
                notifications: true,
            },
        };
        validate_settings(&settings)?;
        let mut config = store.config().await?;
        config.database_path = database.into();
        // Desktop admission is independently adjustable up to 32; transport caps remain global.
        config.max_concurrent_jobs = 32;
        let transport = Arc::new(HttpTransport::new(&config).map_err(|e| invalid(e.to_string()))?);
        let engine = Arc::new(Engine::new(
            config,
            transport,
            store.clone(),
            Arc::new(LocalFiles),
        )?);
        let session = Arc::new(Self {
            database_path: database.into(),
            engine,
            store,
            queue: Mutex::new(Queue {
                stopping: false,
                tasks: HashMap::new(),
                settings,
                samples: HashMap::new(),
                revision: 0,
            }),
            _lease: Arc::new(lease),
        });
        let weak = Arc::downgrade(&session);
        tokio::spawn(async move { runner(weak).await });
        Ok(session)
    }

    async fn record(&self, id: JobId, queue: &mut Queue) -> EngineResult<Job> {
        let job = self
            .store
            .load_job(id)
            .await?
            .ok_or(EngineError::JobNotFound(id))?;
        self.render(job, queue).await
    }

    async fn render(&self, mut job: JobRecord, queue: &mut Queue) -> EngineResult<Job> {
        let id = job.id;
        let segments = self.store.load_segments(id).await?;
        if job.state != JobState::Completed {
            job.bytes_downloaded = segments.iter().map(|s| s.bytes_downloaded).sum();
        }
        if let Some(bytes) = self.engine.live_full_bytes(id) {
            job.bytes_downloaded = bytes;
        }
        let now = Instant::now();
        // Only live workers need throughput samples. Repeated history lookups
        // must not grow session memory with the total number of persisted jobs.
        let speed = if job.state == JobState::Downloading {
            let sample = queue
                .samples
                .entry(id)
                .or_insert((now, job.bytes_downloaded, 0.0));
            if now.duration_since(sample.0) >= Duration::from_millis(100) {
                sample.2 = job.bytes_downloaded.saturating_sub(sample.1) as f64
                    / now.duration_since(sample.0).as_secs_f64();
                sample.0 = now;
                sample.1 = job.bytes_downloaded;
            }
            sample.2
        } else {
            queue.samples.remove(&id);
            0.0
        };
        Ok(Job {
            id: job.id.to_string(),
            name: job
                .output_path
                .file_name()
                .map(|n| n.to_string_lossy().into())
                .unwrap_or_default(),
            url: job.url.to_string(),
            destination: job.output_path.to_string_lossy().into(),
            state: format!("{:?}", job.state),
            bytes: job.bytes_downloaded as f64,
            total: job.total_size.map(|n| n as f64),
            speed,
            priority: job.priority,
            retries: job.retry_count,
            workers: self.engine.active_workers(id),
            error: job.error_message,
            etag: job.remote_identity.etag,
            response_status: self.engine.response_diagnostic(id).map(|d| d.0),
            response_latency_ms: self.engine.response_diagnostic(id).map(|d| d.1),
            segments: segments
                .into_iter()
                .map(|s| Segment {
                    start: s.start as f64,
                    end: s.end as f64,
                    bytes: s.bytes_downloaded as f64,
                    state: format!("{:?}", s.state),
                })
                .collect(),
        })
    }

    async fn stop(&self, job: &JobRecord, queue: &mut Queue, cancel: bool) -> EngineResult<()> {
        if !matches!(job.state, JobState::Completed | JobState::Cancelled) {
            if cancel {
                self.engine.cancel(job.id).await?;
            } else {
                self.engine.pause(job.id).await?;
            }
        }
        if let Some(task) = queue.tasks.remove(&job.id) {
            task.await
                .map_err(|e| invalid(format!("worker failed: {e}")))?;
        }
        if cancel {
            self.engine.forget_session(job.id);
            match tokio::fs::remove_file(&job.partial_path).await {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(invalid(format!("cannot remove partial: {e}"))),
            }
        }
        Ok(())
    }
}

async fn runner(session: Weak<Session>) {
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let Some(session) = session.upgrade() else {
            break;
        };
        if let Err(e) = admit(&session).await {
            tracing::error!(error=%e, "queue admission failed");
        }
    }
}

impl okytes_browser_bridge::Handler for Session {
    fn handle(
        &self,
        request: okytes_browser_bridge::protocol::Request,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = okytes_browser_bridge::protocol::Response> + Send + '_,
        >,
    > {
        Box::pin(async move {
            use okytes_browser_bridge::protocol::{Operation, Response};
            if request.validate().is_err() {
                return Response::reply(request.id, Err("invalid_request".into()));
            }
            let result = match request.operation {
                Operation::Ping => Ok(None),
                Operation::Capture(capture) => self
                    .capture(request.id.clone(), capture)
                    .await
                    .map(Some)
                    .map_err(|_| "capture_rejected".into()),
            };
            Response::reply(request.id, result)
        })
    }
}

async fn admit(session: &Arc<Session>) -> EngineResult<()> {
    let mut queue = session.queue.lock().await;
    if queue.stopping {
        return Ok(());
    }
    queue.tasks.retain(|_, task| !task.is_finished());
    let limit = queue.settings.concurrency as usize;
    if queue.tasks.len() >= limit {
        return Ok(());
    }
    for job in session.store.runnable().await? {
        if queue.tasks.len() >= limit {
            break;
        }
        if queue.tasks.contains_key(&job.id) {
            continue;
        }
        let engine = session.engine.clone();
        let lease = session._lease.clone();
        let id = job.id;
        queue.tasks.insert(
            id,
            tokio::spawn(async move {
                let _lease = lease;
                if let Err(e) = engine.execute_job(id).await {
                    tracing::debug!(%id,error=%e,"transfer stopped");
                }
            }),
        );
    }
    Ok(())
}

fn validate_settings(settings: &Settings) -> EngineResult<()> {
    if settings.version != 1
        || !(1..=32).contains(&settings.concurrency)
        || !["light", "dark", "system"].contains(&settings.theme.as_str())
        || settings.destination.len() > 4096
        || !Path::new(&settings.destination).is_absolute()
    {
        return Err(invalid("settings require version 1, an absolute destination, 1–32 downloads and light/dark/system theme"));
    }
    Ok(())
}

impl DesktopService for Session {
    fn submit(&self, input: okytes_core::desktop::Submit) -> Reply<'_, String> {
        Box::pin(async move {
            let queue = self.queue.lock().await;
            if queue.stopping {
                return Err(invalid("service is shutting down"));
            }
            let destination = input
                .destination
                .unwrap_or_else(|| queue.settings.destination.clone());
            if destination.len() > 4096 || !Path::new(&destination).is_absolute() {
                return Err(invalid("absolute destination required"));
            }
            let url = url::Url::parse(&input.url).map_err(|_| invalid("invalid source URL"))?;
            if input.filename.as_ref().is_some_and(|name| {
                name.len() > 200 || name != &okytes_core::engine::sanitize(name)
            }) {
                return Err(invalid("safe basename required"));
            }
            let conflict_strategy = match input.conflict.as_str() {
                "rename" => ConflictStrategy::Rename,
                "fail" => ConflictStrategy::Fail,
                "overwrite" => ConflictStrategy::Overwrite,
                _ => return Err(invalid("invalid conflict strategy")),
            };
            let id = self
                .engine
                .add(DownloadRequest {
                    url,
                    output_dir: PathBuf::from(destination),
                    filename: input.filename,
                    expected_hash: input.sha256,
                    conflict_strategy,
                    headers: vec![],
                    priority: 0,
                })
                .await?;
            Ok(id.to_string())
        })
    }
    fn pause_all(&self) -> Reply<'_, ()> {
        Box::pin(async move {
            let mut queue = self.queue.lock().await;
            loop {
                let jobs = self.store.runnable().await?;
                if jobs.is_empty() {
                    break;
                }
                for job in jobs {
                    if let Err(error) = self.engine.pause(job.id).await {
                        let state = self.engine.status(job.id).await?.state;
                        if state.is_active() || state == JobState::Queued {
                            return Err(error);
                        }
                    }
                }
            }
            for (_, task) in queue.tasks.drain() {
                task.await
                    .map_err(|e| invalid(format!("worker failed: {e}")))?;
            }
            queue.revision += 1;
            Ok(())
        })
    }
    fn add(&self, input: AddInput) -> Reply<'_, Vec<String>> {
        Box::pin(async move {
            if input.urls.is_empty() || input.urls.len() > 100 {
                return Err(invalid("import must contain 1–100 URLs"));
            }
            let mut queue = self.queue.lock().await;
            let destination = input
                .destination
                .unwrap_or_else(|| queue.settings.destination.clone());
            if destination.len() > 4096 || !Path::new(&destination).is_absolute() {
                return Err(invalid("choose an absolute destination directory"));
            }
            let urls: Vec<url::Url> = input
                .urls
                .iter()
                .map(|s| {
                    let url = url::Url::parse(s).map_err(|_| invalid("invalid URL"))?;
                    if s.len() > 8192
                        || !matches!(url.scheme(), "http" | "https")
                        || !url.username().is_empty()
                        || url.password().is_some()
                    {
                        return Err(invalid(
                            "HTTP(S) URLs without embedded credentials required",
                        ));
                    }
                    Ok(url)
                })
                .collect::<EngineResult<_>>()?;
            if !input.allow_duplicates {
                let mut offset = 0;
                loop {
                    let page = self.store.page(offset).await?;
                    if page.iter().any(|j| urls.contains(&j.url)) {
                        return Err(invalid("duplicate URL: choose Add duplicates to continue"));
                    }
                    if page.len() < 256 {
                        break;
                    }
                    offset += page.len();
                }
                for (index, url) in urls.iter().enumerate() {
                    if urls[..index].contains(url) {
                        return Err(invalid(
                            "duplicate URL in import: choose Add duplicates to continue",
                        ));
                    }
                }
            }
            let mut ids = Vec::new();
            for url in urls {
                ids.push(
                    self.engine
                        .add(DownloadRequest {
                            url,
                            output_dir: PathBuf::from(&destination),
                            filename: None,
                            expected_hash: None,
                            conflict_strategy: ConflictStrategy::Rename,
                            headers: vec![],
                            priority: 0,
                        })
                        .await?
                        .to_string(),
                );
            }
            queue.revision += 1;
            Ok(ids)
        })
    }
    fn list(&self, offset: u32) -> Reply<'_, Snapshot> {
        Box::pin(async move {
            if offset > 1_000_000 {
                return Err(invalid("history offset exceeds limit"));
            }
            let mut queue = self.queue.lock().await;
            let page = self.store.page(offset as usize).await?;
            let next = (page.len() == 256).then_some(offset + 256);
            let mut jobs = Vec::with_capacity(page.len());
            for job in page {
                jobs.push(self.render(job, &mut queue).await?);
            }
            queue.revision += 1;
            // Sampling memory only follows this page and the bounded worker set.
            let active: Vec<_> = queue.tasks.keys().copied().collect();
            queue.samples.retain(|id, _| active.contains(id));
            Ok(Snapshot {
                revision: queue.revision.to_string(),
                jobs,
                next,
            })
        })
    }
    fn get(&self, value: String) -> Reply<'_, Job> {
        Box::pin(async move {
            self.record(id(&value)?, &mut *self.queue.lock().await)
                .await
        })
    }
    fn control(&self, value: String, action: Action) -> Reply<'_, ()> {
        Box::pin(async move {
            let id = id(&value)?;
            let mut queue = self.queue.lock().await;
            let mut job = self.engine.status(id).await?;
            match action {
                Action::Pause => self.stop(&job, &mut queue, false).await?,
                Action::Cancel => self.stop(&job, &mut queue, true).await?,
                Action::Resume => {
                    if job.state == JobState::Queued {
                        return Ok(());
                    }
                    if !job.state.is_resumable() {
                        return Err(invalid("only paused or failed jobs can be resumed"));
                    }
                    if let Some(task) = queue.tasks.remove(&id) {
                        task.await.map_err(|e| invalid(e.to_string()))?;
                    }
                    self.engine.resume(id).await?;
                }
                Action::Restart => {
                    if job.requires_session {
                        return Err(invalid("Capture this download again from the browser to supply a fresh session"));
                    }
                    self.stop(&job, &mut queue, true).await?;
                    let directory = job
                        .output_path
                        .parent()
                        .ok_or_else(|| invalid("destination has no parent"))?;
                    self.engine
                        .add(DownloadRequest {
                            url: job.url,
                            output_dir: directory.into(),
                            filename: job
                                .output_path
                                .file_name()
                                .map(|n| n.to_string_lossy().into()),
                            expected_hash: job.expected_hash,
                            conflict_strategy: ConflictStrategy::Rename,
                            headers: vec![],
                            priority: job.priority,
                        })
                        .await?;
                }
                Action::Remove => {
                    self.stop(&job, &mut queue, true).await?;
                    self.store.delete_job(id).await?;
                    queue.samples.remove(&id);
                }
                Action::Priority(priority) => {
                    if !(-100..=100).contains(&priority)
                        || job.state != JobState::Queued
                        || queue.tasks.contains_key(&id)
                    {
                        return Err(invalid(
                            "priority can be changed on queued jobs only (-100 to 100)",
                        ));
                    }
                    job.priority = priority;
                    self.store.save_job(&job).await?;
                }
                Action::Rename(name) => {
                    if queue.tasks.contains_key(&id)
                        || !matches!(
                            job.state,
                            JobState::Paused | JobState::Failed | JobState::Queued
                        )
                    {
                        return Err(invalid("pause a download before renaming it"));
                    }
                    if name.is_empty()
                        || name.len() > 200
                        || name != okytes_core::engine::sanitize(&name)
                    {
                        return Err(invalid(
                            "use a safe filename without path separators or reserved names",
                        ));
                    }
                    let path = job.output_path.with_file_name(name);
                    if path.exists() {
                        return Err(invalid("destination filename already exists"));
                    }
                    job.output_path = path;
                    job.use_server_filename = false;
                    self.store.save_job(&job).await?;
                }
            }
            queue.revision += 1;
            Ok(())
        })
    }
    fn settings(&self) -> Reply<'_, Settings> {
        Box::pin(async move { Ok(self.queue.lock().await.settings.clone()) })
    }
    fn set_settings(&self, settings: Settings) -> Reply<'_, ()> {
        Box::pin(async move {
            validate_settings(&settings)?;
            tokio::fs::create_dir_all(&settings.destination)
                .await
                .map_err(|e| invalid(format!("cannot create destination: {e}")))?;
            let mut queue = self.queue.lock().await;
            self.store
                .set_preference(
                    "desktop-v1",
                    &serde_json::to_string(&settings).map_err(|e| invalid(e.to_string()))?,
                )
                .await?;
            queue.settings = settings;
            Ok(())
        })
    }
    fn subscribe(&self) -> Reply<'_, tokio::sync::broadcast::Receiver<EngineEvent>> {
        self.engine.subscribe()
    }
}
