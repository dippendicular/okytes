//! Embedded queue owner. Platform adapters contain no scheduling or persistence rules.
//! A single admission mutex prevents controls racing worker startup. Controls wait
//! for worker quiescence before mutation; snapshots never report speculative bytes.
use okytes_core::store::JobStore;
use okytes_core::{
    desktop::{
        Action, AddInput, Connection, DesktopService, Job, Reply, Segment, Settings, Snapshot,
        Telemetry,
    },
    engine::Engine,
    error::{EngineError, EngineResult},
    service::{DownloadService, EngineEvent},
    state::JobState,
    types::{ConflictStrategy, DownloadRequest, JobId, JobRecord},
};
use okytes_persistence::{files::LocalFiles, EngineLease, SqliteStore};
use okytes_torrent_types::TorrentService;
use okytes_transport_http::HttpTransport;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Weak},
    time::{Duration, Instant},
};
use tokio::{sync::Mutex, task::JoinHandle};
use uuid::Uuid;
/// Portable app backend includes both HTTP controls and torrent capabilities.
pub trait AppService: DesktopService + TorrentService {}
impl<T: DesktopService + TorrentService> AppService for T {}

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
    torrent: Arc<okytes_transport_torrent::Adapter>,
    media: Arc<okytes_transport_media::MediaTransport>,
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

fn organized_destination(root: &str, url: &url::Url) -> PathBuf {
    let name = url
        .path_segments()
        .and_then(Iterator::last)
        .unwrap_or_default();
    let extension = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let folder = match extension.as_str() {
        "mp4" | "mkv" | "webm" | "mov" | "avi" => "Video",
        "mp3" | "flac" | "wav" | "ogg" | "m4a" => "Music",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "Images",
        "zip" | "rar" | "7z" | "gz" | "tar" | "bz2" | "xz" => "Archives",
        "pdf" | "doc" | "docx" | "txt" | "epub" | "csv" | "xlsx" | "pptx" => "Documents",
        "exe" | "msi" | "dmg" | "pkg" | "deb" | "rpm" | "appimage" => "Programs",
        _ => "Other",
    };
    PathBuf::from(root).join("Okytes").join(folder)
}

impl Session {
    /// Clone the handle to this session's single serialized profile writer for
    /// protocol adapters owned by the same process.
    pub fn profile_store(&self) -> Arc<SqliteStore> {
        self.store.clone()
    }
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
        config.traffic = queue.settings.traffic.clone();
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
        settings.traffic = config.traffic.clone();
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
        self.engine.set_traffic(settings.traffic.clone())?;
        self.torrent.set_traffic(&settings.traffic)?;
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
            if job.protocol == "torrent" {
                if job.state == JobState::Verifying {
                    self.torrent.stop_after_publication(*id)?;
                }
                if job.state != JobState::Verifying {
                    self.torrent.pause(*id).await?;
                    if job.state.is_active() || job.state == JobState::Queued {
                        resume.push(*id);
                    }
                }
                continue;
            }
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
                if self.engine.status(id).await?.protocol == "torrent" {
                    self.torrent.resume(id).await?;
                } else {
                    self.engine.resume(id).await?;
                }
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
        let source = url::Url::parse(&capture.url).map_err(|_| invalid("invalid source"))?;
        let result = self
            .engine
            .add_captured(
                DownloadRequest {
                    output_dir: organized_destination(&queue.settings.destination, &source),
                    url: source,
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
        // A losing contender must not migrate or create a writable store before
        // discovering that another process already owns its engine.
        let lease = Arc::new(SqliteStore::acquire_engine(database)?);
        let store = Arc::new(SqliteStore::open(database)?);
        let mut settings = match store.preference("desktop-v1").await? {
            Some(value) => serde_json::from_str(&value).map_err(|e| invalid(e.to_string()))?,
            None => Settings {
                traffic: store.config().await?.traffic,
                version: 1,
                destination: destination.to_string_lossy().into(),
                concurrency: 4,
                theme: "system".into(),
                notifications: true,
                confirm_remove: true,
                remove_files: false,
                animations: true,
                density: "comfortable".into(),
                accent: "blue".into(),
                shortcut_add: "Shift+N".into(),
                shortcut_search: "Shift+F".into(),
                shortcut_help: "Shift+?".into(),
                details_on_start: None,
                completion: None,
            },
        };
        validate_settings(&settings)?;
        let mut config = store.config().await?;
        // CLI profile edits and desktop/API changes share one authoritative policy.
        settings.traffic = config.traffic.clone();
        validate_settings(&settings)?;
        config.database_path = database.into();
        // Desktop admission is independently adjustable up to 32; transport caps remain global.
        config.max_concurrent_jobs = 32;
        config.max_segments_per_job = 12;
        config.max_connections_per_origin = 8;
        config.connect_timeout_secs = config.connect_timeout_secs.min(10);
        config.read_timeout_secs = config.read_timeout_secs.min(30);
        config.max_retries = config.max_retries.min(4);
        config.retry_max_delay_ms = config.retry_max_delay_ms.min(10_000);
        config.traffic = settings.traffic.clone();
        let media = okytes_transport_media::MediaTransport::new(Arc::new(
            HttpTransport::new(&config).map_err(|e| invalid(e.to_string()))?,
        ));
        let transport = Arc::new(okytes_transport_ftp::router::Router {
            files: Arc::new(
                okytes_transport_ftp::FileTransport::new(
                    &config,
                    database.with_extension("known_hosts"),
                )
                .map_err(|e| invalid(e.to_string()))?,
            ),
            fallback: media.clone(),
        });
        let engine = Arc::new(Engine::new(
            config,
            transport,
            store.clone(),
            Arc::new(LocalFiles),
        )?);
        let torrent = okytes_transport_torrent::Adapter::new(
            store.clone(),
            database.with_extension("torrent-data"),
            PathBuf::from(&settings.destination),
            engine.traffic_controller(),
            settings.traffic.clone(),
            engine.event_sender(),
        )?;
        torrent.retain_lease(lease.clone())?;
        let session = Arc::new(Self {
            database_path: database.into(),
            engine,
            torrent,
            media,
            store,
            queue: Mutex::new(Queue {
                stopping: false,
                tasks: HashMap::new(),
                settings,
                samples: HashMap::new(),
                revision: 0,
            }),
            _lease: lease,
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
        if job.protocol == "http" && job.state != JobState::Completed {
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
            protocol: job.protocol.clone(),
            id: job.id.to_string(),
            name: job
                .output_path
                .file_name()
                .map(|n| n.to_string_lossy().into())
                .unwrap_or_default(),
            url: job.url.to_string(),
            destination: job.output_path.to_string_lossy().into(),
            created_at: job.created_at.to_rfc3339(),
            updated_at: job.updated_at.to_rfc3339(),
            state: if job.protocol == "torrent"
                && job.state == JobState::Completed
                && self.torrent.is_seeding(id)?
            {
                "Seeding".into()
            } else {
                format!("{:?}", job.state)
            },
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

    /// Persist settings and apply traffic policy while the caller holds the queue lock.
    async fn apply_settings(&self, queue: &mut Queue, settings: Settings) -> EngineResult<()> {
        let mut config = self.store.config().await?;
        config.traffic = settings.traffic.clone();
        self.store
            .set_preferences(vec![
                (
                    "desktop-v1".into(),
                    serde_json::to_string(&settings).map_err(|e| invalid(e.to_string()))?,
                ),
                (
                    "engine".into(),
                    serde_json::to_string(&config).map_err(|e| invalid(e.to_string()))?,
                ),
            ])
            .await?;
        self.engine.set_traffic(settings.traffic.clone())?;
        self.torrent.set_traffic(&settings.traffic)?;
        queue.settings = settings;
        Ok(())
    }

    /// Drop a removed job's speed limit and completion choice so the bounded
    /// lists (128 entries each) do not fill with entries for deleted jobs.
    async fn forget_preferences(&self, id: &str, queue: &mut Queue) -> EngineResult<()> {
        let mut settings = queue.settings.clone();
        let limits = settings.traffic.job_limits.len();
        settings.traffic.job_limits.retain(|limit| limit.id != id);
        let actions = settings.completion.as_ref().map_or(0, Vec::len);
        if let Some(completion) = settings.completion.as_mut() {
            completion.retain(|action| action.id != id);
        }
        if settings.traffic.job_limits.len() == limits
            && settings.completion.as_ref().map_or(0, Vec::len) == actions
        {
            return Ok(());
        }
        self.apply_settings(queue, settings).await
    }

    async fn stop(&self, job: &JobRecord, queue: &mut Queue, cancel: bool) -> EngineResult<()> {
        if job.protocol == "torrent" {
            if cancel {
                self.torrent.cancel(job.id).await?;
            } else {
                self.torrent.pause(job.id).await?;
            }
            if let Some(task) = queue.tasks.remove(&job.id) {
                task.await
                    .map_err(|_| invalid("torrent worker shutdown failed"))?;
            }
            if cancel && job.partial_path.exists() {
                tokio::fs::remove_dir_all(&job.partial_path)
                    .await
                    .map_err(|e| invalid(e.to_string()))?;
            }
            return Ok(());
        }
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
    if queue.stopping || !session.engine.queue_allowed()? {
        return Ok(());
    }
    queue.tasks.retain(|_, task| !task.is_finished());
    let limit = queue.settings.concurrency as usize;
    if queue.tasks.len() >= limit {
        return Ok(());
    }
    let mut torrents = 0;
    for id in queue.tasks.keys() {
        if session.engine.status(*id).await?.protocol == "torrent" {
            torrents += 1;
        }
    }
    for job in session.store.runnable().await? {
        if queue.tasks.len() >= limit {
            break;
        }
        if queue.tasks.contains_key(&job.id) {
            continue;
        }
        if job.protocol == "torrent" {
            if torrents >= 8 {
                continue;
            }
            torrents += 1;
        }
        let engine = session.engine.clone();
        let torrent = session.torrent.clone();
        let protocol = job.protocol.clone();
        let lease = session._lease.clone();
        let id = job.id;
        queue.tasks.insert(
            id,
            tokio::spawn(async move {
                let _lease = lease;
                let result = if protocol == "torrent" {
                    torrent.execute_job(id).await
                } else {
                    engine.execute_job(id).await
                };
                if let Err(e) = result {
                    tracing::debug!(%id,error=%e,"transfer stopped");
                }
            }),
        );
    }
    Ok(())
}

fn validate_settings(settings: &Settings) -> EngineResult<()> {
    settings.traffic.validate()?;
    if settings.version != 1
        || !(1..=32).contains(&settings.concurrency)
        || !["light", "dark", "system"].contains(&settings.theme.as_str())
        || !["comfortable", "compact"].contains(&settings.density.as_str())
        || !["blue", "violet", "teal", "rose"].contains(&settings.accent.as_str())
        || ![
            &settings.shortcut_add,
            &settings.shortcut_search,
            &settings.shortcut_help,
        ]
        .iter()
        .all(|value| value.starts_with("Shift+") && value.len() <= 32)
        || settings.destination.len() > 4096
        || !Path::new(&settings.destination).is_absolute()
        || settings.completion.as_ref().is_some_and(|actions| {
            let mut ids = std::collections::HashSet::new();
            actions.len() > 128
                || actions
                    .iter()
                    .any(|a| {
                        Uuid::parse_str(&a.id).is_err()
                            || !ids.insert(&a.id)
                            || a.upload.as_deref().is_some_and(|p| {
                                !matches!(p, "gdrive" | "dropbox" | "onedrive")
                            })
                    })
        })
    {
        return Err(invalid("settings require version 1, an absolute destination, 1–32 downloads and light/dark/system theme"));
    }
    Ok(())
}

impl DesktopService for Session {
    fn authorize_file(
        &self,
        id: String,
        credentials: okytes_core::desktop::FileCredentials,
    ) -> Reply<'_, ()> {
        Box::pin(async move {
            let _queue = self.queue.lock().await;
            self.engine
                .authorize_file(
                    id.parse().map_err(|_| invalid("invalid job ID"))?,
                    credentials,
                )
                .await
        })
    }
    fn inspect_media(&self, url: String) -> Reply<'_, Vec<okytes_core::desktop::MediaChoice>> {
        Box::pin(async move {
            if url.len() > 8192 {
                return Err(invalid("media URL exceeds bound"));
            }
            self.media
                .inspect(&url)
                .await
                .map_err(|e| invalid(e.to_string()))
        })
    }
    fn expand_playlist(&self, url: String) -> Reply<'_, Vec<String>> {
        Box::pin(async move { okytes_core::playlist::expand(&*self.media, &url).await })
    }
    fn crawl(
        &self,
        input: okytes_core::desktop::CrawlInput,
    ) -> Reply<'_, okytes_core::desktop::CrawlResult> {
        Box::pin(async move { okytes_core::crawl::crawl(&*self.media, input).await })
    }
    fn inspect_zip(&self, url: String) -> Reply<'_, okytes_core::desktop::ZipListing> {
        Box::pin(async move {
            let parsed = url::Url::parse(&url).map_err(|_| invalid("invalid URL"))?;
            if url.len() > 8192
                || !matches!(parsed.scheme(), "http" | "https")
                || !parsed.username().is_empty()
                || parsed.password().is_some()
            {
                return Err(invalid("ZIP preview needs an HTTP(S) URL without credentials"));
            }
            okytes_core::archive::zip_listing(&*self.media, &parsed).await
        })
    }
    fn submit(&self, input: okytes_core::desktop::Submit) -> Reply<'_, String> {
        Box::pin(async move {
            let queue = self.queue.lock().await;
            if queue.stopping {
                return Err(invalid("service is shutting down"));
            }
            let requested_destination = input.destination;
            let url = url::Url::parse(&input.url).map_err(|_| invalid("invalid source URL"))?;
            let destination = requested_destination
                .map(PathBuf::from)
                .unwrap_or_else(|| organized_destination(&queue.settings.destination, &url));
            if destination.to_string_lossy().len() > 4096 || !destination.is_absolute() {
                return Err(invalid("absolute destination required"));
            }
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
            if input.credentials.is_some() && !matches!(url.scheme(), "ftp" | "ftps" | "sftp") {
                return Err(invalid("file credentials require a file-protocol URL"));
            }
            let headers = input
                .credentials
                .map(|c| c.into_headers())
                .unwrap_or_default();
            let id = self
                .engine
                .add(DownloadRequest {
                    url,
                    output_dir: destination,
                    filename: input.filename,
                    expected_hash: input.sha256,
                    conflict_strategy,
                    headers,
                    priority: 0,
                })
                .await?;
            Ok(id.to_string())
        })
    }
    fn pause_all(&self) -> Reply<'_, ()> {
        Box::pin(async move {
            let mut queue = self.queue.lock().await;
            for id in queue.tasks.keys() {
                let job = self.engine.status(*id).await?;
                if job.protocol == "torrent" && job.state == JobState::Completed {
                    self.torrent.pause(*id).await?;
                }
            }
            loop {
                let jobs = self.store.runnable().await?;
                if jobs.is_empty() {
                    break;
                }
                for job in jobs {
                    let result = if job.protocol == "torrent" {
                        self.torrent.pause(job.id).await
                    } else {
                        self.engine.pause(job.id).await
                    };
                    if let Err(error) = result {
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
            let requested_destination = input.destination;
            let urls: Vec<url::Url> = input
                .urls
                .iter()
                .map(|s| {
                    let url = url::Url::parse(s).map_err(|_| invalid("invalid URL"))?;
                    if s.len() > 8192
                        || !matches!(
                            url.scheme(),
                            "http"
                                | "https"
                                | "ftp"
                                | "ftps"
                                | "sftp"
                                | "hls+http"
                                | "hls+https"
                                | "dash+http"
                                | "dash+https"
                        )
                        || !url.username().is_empty()
                        || url.password().is_some()
                    {
                        return Err(invalid(
                            "Supported URLs without embedded credentials required",
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
                let destination = requested_destination
                    .as_deref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| organized_destination(&queue.settings.destination, &url));
                if destination.to_string_lossy().len() > 4096 || !destination.is_absolute() {
                    return Err(invalid("choose an absolute destination directory"));
                }
                ids.push(
                    self.engine
                        .add(DownloadRequest {
                            url,
                            output_dir: destination,
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
            let page_full = page.len() == 256;
            let mut next = None;
            let mut encoded = 128usize;
            let mut jobs = Vec::with_capacity(page.len());
            for job in page {
                let job = self.render(job, &mut queue).await?;
                let size = serde_json::to_vec(&job)
                    .map_err(|_| invalid("snapshot encoding failed"))?
                    .len()
                    + 1;
                if !jobs.is_empty() && encoded + size > 4 * 1024 * 1024 {
                    next = Some(offset + jobs.len() as u32);
                    break;
                }
                if size > 4 * 1024 * 1024 {
                    return Err(invalid("job diagnostics exceed snapshot bound"));
                }
                encoded += size;
                jobs.push(job);
            }
            if next.is_none() && page_full {
                next = Some(offset + jobs.len() as u32);
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
    fn telemetry(&self, value: String) -> Reply<'_, Telemetry> {
        Box::pin(async move {
            let id = id(&value)?;
            let record = self
                .store
                .load_job(id)
                .await?
                .ok_or(EngineError::JobNotFound(id))?;
            let last_modified = record.remote_identity.last_modified.clone();
            let hash_expected = record.expected_hash.is_some();
            let verified = record.verified_sha256.is_some();
            let job = self.render(record, &mut *self.queue.lock().await).await?;
            // Sampled after rendering so durable bytes and live requests are close in time.
            let (links, stats) = self.engine.telemetry(id);
            Ok(Telemetry {
                average_speed: stats.average_speed(job.bytes as u64),
                peak_speed: (stats.peak_speed > 0.0).then_some(stats.peak_speed),
                range_support: stats.range_support,
                concurrency_limit: stats.concurrency_limit,
                last_modified,
                hash_expected,
                verified_bytes: if verified { job.bytes } else { 0.0 },
                connections: links
                    .into_iter()
                    .map(|link| Connection {
                        id: link.id,
                        segment_start: link.segment_start as f64,
                        start: link.start as f64,
                        end: link.end.map(|end| end as f64),
                        bytes: link.received as f64,
                        speed: link.speed,
                        retries: link.attempt,
                        state: link.state.into(),
                        age_ms: link.age.as_secs_f64() * 1000.0,
                    })
                    .collect(),
                job,
            })
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
                    if job.protocol == "torrent" {
                        self.torrent.resume(id).await?;
                    } else {
                        self.engine.resume(id).await?;
                    }
                }
                Action::Restart => {
                    if job.protocol == "torrent" {
                        return Err(invalid("re-add the torrent to start a separate transfer"));
                    }
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
                    self.forget_preferences(&id.to_string(), &mut queue).await?;
                }
                Action::RemoveFiles => {
                    self.stop(&job, &mut queue, true).await?;
                    match tokio::fs::symlink_metadata(&job.output_path).await {
                        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
                            tokio::fs::remove_file(&job.output_path)
                                .await
                                .map_err(|e| {
                                    invalid(format!("cannot remove downloaded file: {e}"))
                                })?;
                        }
                        Ok(metadata) if metadata.is_dir() => {
                            tokio::fs::remove_dir_all(&job.output_path)
                                .await
                                .map_err(|e| {
                                    invalid(format!("cannot remove downloaded folder: {e}"))
                                })?;
                        }
                        Ok(_) => {
                            return Err(invalid(
                                "downloaded item has an unsupported filesystem type",
                            ))
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(invalid(format!("cannot inspect downloaded item: {error}")))
                        }
                    }
                    self.store.delete_job(id).await?;
                    queue.samples.remove(&id);
                    self.forget_preferences(&id.to_string(), &mut queue).await?;
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
                Action::Refresh(link) => {
                    if queue.tasks.contains_key(&id) {
                        return Err(invalid("pause the download before replacing its link"));
                    }
                    if link.len() > 8192 {
                        return Err(invalid("link exceeds 8192 bytes"));
                    }
                    let url = url::Url::parse(link.trim()).map_err(|_| invalid("invalid link"))?;
                    self.engine.refresh_source(id, url).await?;
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
            self.apply_settings(&mut queue, settings).await
        })
    }
    fn subscribe(&self) -> Reply<'_, tokio::sync::broadcast::Receiver<EngineEvent>> {
        self.engine.subscribe()
    }
}

impl TorrentService for Session {
    fn inspect_torrent(
        &self,
        input: okytes_torrent_types::Input,
    ) -> Reply<'_, okytes_torrent_types::Metadata> {
        self.torrent.inspect_torrent(input)
    }
    fn add_torrent(&self, mut input: okytes_torrent_types::Input) -> Reply<'_, String> {
        Box::pin(async move {
            let mut queue = self.queue.lock().await;
            if input.destination.is_none() {
                input.destination = Some(queue.settings.destination.clone());
            }
            let id = self.torrent.add_torrent(input).await?;
            queue.revision += 1;
            Ok(id)
        })
    }
    fn torrent(&self, id: String) -> Reply<'_, okytes_torrent_types::Snapshot> {
        self.torrent.torrent(id)
    }
}
