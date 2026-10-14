//! App-owned torrent executor. The maintained engine owns peer and piece work;
//! verified private data is copied to an atomic publication directory.
use crate::{
    metainfo,
    storage::{Data, Factory},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::Utc;
use librqbit::storage::StorageFactoryExt;
use librqbit::{AddTorrent, AddTorrentOptions, AddTorrentResponse, Session, SessionOptions};
use okytes_core::{
    desktop::Reply,
    error::{EngineError, EngineResult},
    service::{DownloadService, EngineEvent},
    state::JobState,
    store::JobStore,
    traffic::{TrafficController, TrafficPolicy},
    types::*,
};
use okytes_persistence::SqliteStore;
use okytes_torrent_types::{Input, Metadata, Options, Snapshot, TorrentService};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use url::Url;

fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::Config(message.into())
}
fn library(error: impl std::fmt::Display) -> EngineError {
    invalid(format!("torrent operation failed: {error}"))
}

#[derive(Clone, Serialize, Deserialize)]
struct Descriptor {
    magnet: Option<String>,
    options: Options,
    #[serde(default)]
    uploaded_bytes: u64,
    #[serde(default)]
    receipts: Vec<Receipt>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    index: u32,
    hash: String,
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
struct Running {
    data: Arc<Data>,
    session: tokio::sync::OnceCell<Arc<Session>>,
    handle: Mutex<Option<Arc<librqbit::ManagedTorrent>>>,
    cancel: CancellationToken,
    finished: Mutex<Option<Instant>>,
    done: tokio::sync::Notify,
    quiesced: AtomicBool,
    finish_then_stop: AtomicBool,
}
impl Running {
    fn session(&self) -> EngineResult<&Arc<Session>> {
        self.session
            .get()
            .ok_or_else(|| invalid("torrent session is not initialized"))
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.data.stopped.store(true, Ordering::Release);
    }
}

/// Bounded protocol executor, composed by the app with its HTTP executor.
pub struct Adapter {
    store: Arc<SqliteStore>,
    root: PathBuf,
    destination: PathBuf,
    down: Arc<TrafficController>,
    up: Arc<TrafficController>,
    events: broadcast::Sender<EngineEvent>,
    running: Arc<Mutex<HashMap<JobId, Arc<Running>>>>,
    slots: tokio::sync::Semaphore,
    previews: tokio::sync::Semaphore,
    lease: Mutex<Option<Arc<okytes_persistence::EngineLease>>>,
}
impl Adapter {
    /// Reuse the HTTP executor's download controller; upload has a separate shared bucket.
    pub fn new(
        store: Arc<SqliteStore>,
        root: PathBuf,
        destination: PathBuf,
        down: Arc<TrafficController>,
        policy: TrafficPolicy,
        events: broadcast::Sender<EngineEvent>,
    ) -> EngineResult<Arc<Self>> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        librqbit::metainfo_guard::install(|raw| {
            metainfo::validate(raw).map(|_| ()).map_err(Into::into)
        });
        Ok(Arc::new(Self {
            store,
            root,
            destination,
            down,
            up: Arc::new(TrafficController::new(policy.upload_policy())?),
            events,
            running: Arc::new(Mutex::new(HashMap::new())),
            slots: tokio::sync::Semaphore::new(8),
            previews: tokio::sync::Semaphore::new(2),
            lease: Mutex::new(None),
        }))
    }
    /// Keep the profile lease alive until cancelled native I/O has drained.
    pub fn retain_lease(&self, lease: Arc<okytes_persistence::EngineLease>) -> EngineResult<()> {
        *self
            .lease
            .lock()
            .map_err(|_| invalid("lease lock failed"))? = Some(lease);
        Ok(())
    }
    /// Let protected publication finish, then stop sharing during app shutdown.
    pub fn stop_after_publication(&self, id: JobId) -> EngineResult<()> {
        if let Some(running) = self.active(id)? {
            running.finish_then_stop.store(true, Ordering::Release);
        }
        Ok(())
    }
    /// App activity is separate from the generic completed-data state.
    pub fn is_seeding(&self, id: JobId) -> EngineResult<bool> {
        Ok(self.active(id)?.is_some_and(|running| {
            running.data.complete.load(Ordering::Acquire)
                && !running.data.stopped.load(Ordering::Acquire)
        }))
    }
    /// Apply live global upload settings alongside the shared download policy.
    pub fn set_traffic(&self, policy: &TrafficPolicy) -> EngineResult<()> {
        self.up.set(policy.upload_policy())
    }
    async fn record(&self, id: JobId) -> EngineResult<JobRecord> {
        self.store
            .load_job(id)
            .await?
            .filter(|j| j.protocol == "torrent")
            .ok_or_else(|| invalid("torrent job not found"))
    }
    async fn descriptor(&self, id: JobId) -> EngineResult<(Descriptor, Vec<u8>)> {
        let (json, raw) = self
            .store
            .torrent_record(id)
            .await?
            .ok_or_else(|| invalid("torrent state not found"))?;
        Ok((serde_json::from_str(&json).map_err(library)?, raw))
    }
    async fn save_descriptor(&self, id: JobId, value: &Descriptor, raw: &[u8]) -> EngineResult<()> {
        self.store
            .set_torrent_record(id, &serde_json::to_string(value).map_err(library)?, raw)
            .await
    }
    fn active(&self, id: JobId) -> EngineResult<Option<Arc<Running>>> {
        Ok(self
            .running
            .lock()
            .map_err(|_| invalid("torrent registry lock failed"))?
            .get(&id)
            .cloned())
    }
    async fn state(&self, id: JobId, state: JobState) -> EngineResult<()> {
        let old = self.record(id).await?.state;
        self.store.transition_job(id, state).await?;
        let _ = self.events.send(EngineEvent::JobStateChanged {
            job_id: id,
            old_state: old,
            new_state: state,
        });
        Ok(())
    }
    /// Execute a queued job; admission remains with the app queue owner.
    pub async fn execute_job(&self, id: JobId) -> EngineResult<()> {
        let permit = self
            .slots
            .try_acquire()
            .map_err(|_| invalid("torrent admission limit reached"))?;
        if self.active(id)?.is_some() {
            return Err(invalid("torrent job is already executing"));
        }
        let mut job = self.record(id).await?;
        if job.partial_path != self.root.join(id.to_string()) {
            return Err(invalid("unsafe private data location"));
        }
        let (mut descriptor, mut raw) = self.descriptor(id).await?;
        if job.state == JobState::Completed {
            return Ok(());
        }
        {
            let mut lease = self
                .lease
                .lock()
                .map_err(|_| invalid("lease lock failed"))?;
            if lease.is_none() {
                *lease = Some(Arc::new(SqliteStore::acquire_engine(
                    self.store.database_path(),
                )?));
            }
        }
        if job.state.is_active() {
            self.state(id, JobState::Queued).await?;
            job.state = JobState::Queued;
        }
        if job.state != JobState::Queued {
            return Err(invalid("torrent must be queued before execution"));
        }
        let data = Arc::new(Data {
            id,
            root: job.partial_path.clone(),
            store: self.store.clone(),
            options: descriptor.options.clone(),
            recipe: Mutex::new(None),
            verified: Mutex::new(vec![]),
            stopped: AtomicBool::new(false),
            complete: AtomicBool::new(false),
            sealed: AtomicBool::new(false),
            quota_exhausted: AtomicBool::new(false),
            io_gate: std::sync::RwLock::new(()),
        });
        let cancel = CancellationToken::new();
        let _cancel_on_drop = CancelOnDrop(cancel.clone());
        let running = Arc::new(Running {
            data: data.clone(),
            session: tokio::sync::OnceCell::new(),
            done: tokio::sync::Notify::new(),
            quiesced: AtomicBool::new(false),
            finish_then_stop: AtomicBool::new(false),
            handle: Mutex::new(None),
            cancel,
            finished: Mutex::new(None),
        });
        {
            let mut registry = self
                .running
                .lock()
                .map_err(|_| invalid("torrent registry lock failed"))?;
            if registry.len() >= 8 || registry.contains_key(&id) {
                return Err(invalid("torrent admission limit reached"));
            }
            registry.insert(id, running.clone());
        }
        let mut guard = ExecutionGuard {
            running: running.clone(),
            registry: self.running.clone(),
            down: self.down.clone(),
            up: self.up.clone(),
            lease: self
                .lease
                .lock()
                .map_err(|_| invalid("lease lock failed"))?
                .clone(),
            armed: true,
        };
        let result = async {
            if running.data.stopped.load(Ordering::Acquire) {
                return Ok(());
            }
            self.state(id, JobState::Probing).await?;
            self.store.torrent_begin(id).await?;
            let session = session(&data.root, &descriptor.options, running.cancel.clone()).await?;
            running
                .session
                .set(session)
                .map_err(|_| invalid("duplicate torrent session"))?;
            if running.data.stopped.load(Ordering::Acquire) {
                return Ok(());
            }
            tokio::select! {result=self.transfer(&mut job,&mut descriptor,&mut raw,&running)=>result, result=watch_control(&self.store,id)=>{running.data.stopped.store(true,Ordering::Release);running.cancel.cancel();result}}
        }
        .await;
        if let Some(handle) = running
            .handle
            .lock()
            .map_err(|_| invalid("torrent handle lock failed"))?
            .clone()
        {
            descriptor.uploaded_bytes = handle.stats().uploaded_bytes;
        }
        quiesce(&running).await?;
        cleanup_cancelled(&running.data).await?;
        self.down.release(id);
        self.up.release(id);
        self.save_descriptor(id, &descriptor, &raw).await?;
        if let Err(error) = &result {
            if self.record(id).await?.state.is_active() {
                self.store
                    .record_diagnostic(id, Some(error.to_string()), false)
                    .await?;
                self.state(id, JobState::Failed).await?;
                let _ = self.events.send(EngineEvent::JobFailed {
                    job_id: id,
                    error: error.to_string(),
                });
            }
        }
        self.store.torrent_idle(id).await?;
        guard.armed = false;
        self.running
            .lock()
            .map_err(|_| invalid("torrent registry lock failed"))?
            .remove(&id);
        running.quiesced.store(true, Ordering::Release);
        running.done.notify_waiters();
        drop(permit);
        result
    }
    async fn transfer(
        &self,
        job: &mut JobRecord,
        descriptor: &mut Descriptor,
        raw: &mut Vec<u8>,
        running: &Arc<Running>,
    ) -> EngineResult<()> {
        if raw.is_empty() {
            *raw = tokio::select! {result=resolve(running.session()?,descriptor.magnet.as_deref().ok_or_else(||invalid("missing magnet"))?,&descriptor.options)=>result?,_ = running.cancel.cancelled()=>return Ok(())};
            metainfo::validate(raw)?;
            self.save_descriptor(job.id, descriptor, raw).await?;
        }
        let gate = Arc::new(Gate {
            running: Arc::downgrade(running),
            down: self.down.clone(),
            up: self.up.clone(),
        });
        let response = running
            .session()?
            .add_torrent(
                AddTorrent::from_bytes(raw.clone()),
                Some(AddTorrentOptions {
                    paused: true,
                    overwrite: true,
                    output_folder: Some(job.partial_path.to_string_lossy().into_owned()),
                    only_files: descriptor
                        .options
                        .files
                        .as_ref()
                        .map(|f| f.iter().map(|i| *i as usize).collect()),
                    initial_peers: Some(
                        descriptor
                            .options
                            .initial_peers
                            .iter()
                            .map(|p| p.parse())
                            .collect::<Result<_, _>>()
                            .map_err(library)?,
                    ),
                    peer_limit: Some(descriptor.options.peers as usize),
                    storage_factory: Some(Factory(running.data.clone()).boxed()),
                    payload_gate: Some(gate),
                    disable_pex: !descriptor.options.privacy.pex,
                    ..Default::default()
                }),
            )
            .await
            .map_err(library)?;
        let handle = response
            .into_handle()
            .ok_or_else(|| invalid("torrent engine did not create a job"))?;
        *running
            .handle
            .lock()
            .map_err(|_| invalid("torrent handle lock failed"))? = Some(handle.clone());
        tokio::select! {result=handle.wait_until_initialized()=>result.map_err(library)?,_ = running.cancel.cancelled()=>return Ok(())};
        let bits = handle.verified_pieces().map_err(library)?;
        self.store
            .replace_torrent_pieces(job.id, bits.clone())
            .await?;
        *running
            .data
            .verified
            .lock()
            .map_err(|_| invalid("piece bitmap lock failed"))? = bits;
        running.data.refresh_complete().map_err(library)?;
        job.total_size = Some(running.data.selected_bytes().map_err(library)?);
        job.state = self.record(job.id).await?.state;
        self.store
            .torrent_progress(
                job.id,
                running.data.progress()?,
                running.data.selected_bytes()?,
            )
            .await?;
        if running.data.stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        self.state(job.id, JobState::Downloading).await?;
        running.session()?.unpause(&handle).await.map_err(library)?;
        loop {
            if running.data.stopped.load(Ordering::Acquire) {
                return Ok(());
            }
            if handle.stats().error.is_some() {
                return Err(invalid(
                    "torrent peer engine failed; inspect diagnostics and retry",
                ));
            }
            let mut current = self.record(job.id).await?;
            current.bytes_downloaded = running.data.progress().map_err(library)?;
            current.updated_at = Utc::now();
            self.store
                .torrent_progress(
                    job.id,
                    current.bytes_downloaded,
                    running.data.selected_bytes()?,
                )
                .await?;
            let _ = self.events.send(EngineEvent::Progress(
                okytes_core::service::DownloadProgress {
                    job_id: job.id,
                    state: current.state,
                    bytes_downloaded: current.bytes_downloaded,
                    total_size: current.total_size,
                    throughput_bps: 0.0,
                    active_segments: 0,
                    completed_segments: 0,
                    total_segments: 0,
                },
            ));
            if running.data.complete.load(Ordering::Acquire) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        *running
            .finished
            .lock()
            .map_err(|_| invalid("seed clock lock failed"))? = Some(Instant::now());
        self.state(job.id, JobState::Verifying).await?;
        running.data.sealed.store(true, Ordering::Release);
        self.publish(job, descriptor, raw, &running.data).await?;
        self.state(job.id, JobState::Completed).await?;
        let _ = self
            .events
            .send(EngineEvent::JobCompleted { job_id: job.id });
        loop {
            if running.data.stopped.load(Ordering::Acquire)
                || running.finish_then_stop.load(Ordering::Acquire)
                || running.data.quota_exhausted.load(Ordering::Acquire)
            {
                break;
            }
            if descriptor.options.seed_seconds.is_some_and(|seconds| {
                running
                    .finished
                    .lock()
                    .ok()
                    .and_then(|v| *v)
                    .is_some_and(|start| start.elapsed() >= Duration::from_secs(seconds))
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }
    async fn publish(
        &self,
        job: &JobRecord,
        descriptor: &mut Descriptor,
        raw: &[u8],
        data: &Arc<Data>,
    ) -> EngineResult<()> {
        barrier(data.clone()).await?;
        let recipe = data.recipe().map_err(library)?;
        if job.output_path.exists() {
            if descriptor.receipts.is_empty() {
                return Err(invalid("publication destination already exists"));
            }
            for receipt in &descriptor.receipts {
                let file = recipe
                    .metadata
                    .files
                    .get(receipt.index as usize)
                    .ok_or_else(|| invalid("invalid publication receipt"))?;
                if hash(&job.output_path.join(&file.path)).await? != receipt.hash {
                    return Err(invalid(
                        "published torrent no longer matches verified receipt",
                    ));
                }
            }
            return Ok(());
        }
        let parent = job
            .output_path
            .parent()
            .ok_or_else(|| invalid("missing publication parent"))?;
        tokio::fs::create_dir_all(parent).await.map_err(library)?;
        let staging = parent.join(format!(".okytes-torrent-{}", job.id));
        if staging.exists() {
            tokio::fs::remove_dir_all(&staging).await.map_err(library)?;
        }
        tokio::fs::create_dir(&staging).await.map_err(library)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700))
                .map_err(library)?;
        }
        let mut receipts = vec![];
        for file in recipe.metadata.files.iter().filter(|f| f.selected) {
            let target = staging.join(&file.path);
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(library)?;
            }
            tokio::fs::copy(data.root.join(&file.path), &target)
                .await
                .map_err(library)?;
            let output = tokio::fs::OpenOptions::new()
                .write(true)
                .open(&target)
                .await
                .map_err(library)?;
            output.sync_all().await.map_err(library)?;
            receipts.push(Receipt {
                index: file.index,
                hash: hash(&target).await?,
            });
        }
        descriptor.receipts = receipts;
        self.save_descriptor(job.id, descriptor, raw).await?;
        tokio::fs::rename(staging, &job.output_path)
            .await
            .map_err(library)?;
        #[cfg(unix)]
        {
            std::fs::File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(library)?;
        }
        Ok(())
    }
    async fn stop(&self, id: JobId, state: JobState) -> EngineResult<()> {
        let job = self.record(id).await?;
        if job.state == JobState::Verifying {
            return Err(invalid(
                "torrent publication is in progress; wait for completion",
            ));
        }
        self.store.torrent_control(id, state).await?;
        let new_state = self.record(id).await?.state;
        let _ = self.events.send(EngineEvent::JobStateChanged {
            job_id: id,
            old_state: job.state,
            new_state,
        });
        if let Some(running) = self.active(id)? {
            running.data.stopped.store(true, Ordering::Release);
            running.cancel.cancel();
            if let Some(session) = running.session.get() {
                session.stop().await;
            }
            barrier(running.data.clone()).await?;
        }

        if let Some(running) = self.active(id)? {
            loop {
                let done = running.done.notified();
                if running.quiesced.load(Ordering::Acquire) {
                    break;
                }
                done.await;
            }
        }
        if self.active(id)?.is_none()
            && self
                .lease
                .lock()
                .map_err(|_| invalid("lease lock failed"))?
                .is_some()
        {
            self.store.torrent_idle(id).await?;
        }
        let wait = async {
            loop {
                if !self.store.torrent_active(id).await? {
                    break;
                }
                if self
                    .lease
                    .lock()
                    .map_err(|_| invalid("lease lock failed"))?
                    .is_none()
                {
                    if let Ok(_lease) = SqliteStore::acquire_engine(self.store.database_path()) {
                        self.store.torrent_idle(id).await?;
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok::<_, EngineError>(())
        };
        tokio::time::timeout(Duration::from_secs(5),wait).await.map_err(|_|invalid("torrent owner has not acknowledged stopping; retry or use its daemon connection"))??;
        Ok(())
    }
}

async fn hash(path: &Path) -> EngineResult<String> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await.map_err(library)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 65536];
    loop {
        let read = file.read(&mut buffer).await.map_err(library)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
async fn session(
    root: &Path,
    options: &Options,
    cancel: CancellationToken,
) -> EngineResult<Arc<Session>> {
    Session::new_with_opts(
        root.to_owned(),
        SessionOptions {
            dht: options.privacy.dht.then(|| librqbit::DhtSessionConfig {
                bootstrap_addrs: options.privacy.bootstrap_nodes.clone(),
                port: None,
                persistence: None,
            }),
            bind_device_name: options.privacy.bind_device.clone(),
            listen: Some(librqbit::ListenerOptions {
                listen_addr: options.privacy.listen.parse().map_err(library)?,
                max_pending_incoming_handshake_checks: 32,
                ..Default::default()
            }),
            connect: Some(librqbit::ConnectionOptions {
                proxy_url: options.privacy.peer_proxy.clone(),
                peer_opts: Some(librqbit::PeerConnectionOptions {
                    connect_timeout: Some(Duration::from_secs(5)),
                    read_write_timeout: Some(Duration::from_secs(180)),
                    keep_alive_interval: Some(Duration::from_secs(5)),
                }),
                ..Default::default()
            }),
            disable_local_service_discovery: !options.privacy.local_discovery,
            concurrent_init_limit: Some(1),
            runtime_worker_threads: Some(2),
            peer_limit: Some(options.peers as usize),
            cancellation_token: Some(cancel),
            client_name_and_version: Some("Okytes / librqbit 9.0.1".into()),
            ..Default::default()
        },
    )
    .await
    .map_err(library)
}
fn source(input: &Input) -> EngineResult<(Option<String>, Vec<u8>)> {
    metainfo::options(&input.options)?;
    match (&input.magnet, &input.metainfo) {
        (Some(magnet), None) => {
            if magnet.len() > 8192 {
                return Err(invalid("magnet exceeds 8192 bytes"));
            }
            let uri = Url::parse(magnet).map_err(library)?;
            if uri.query_pairs().any(|(key, _)| key == "so") {
                return Err(invalid(
                    "use explicit file indices instead of magnet so ranges",
                ));
            }
            if librqbit::Magnet::parse(magnet)
                .map_err(library)?
                .as_id20()
                .is_none()
            {
                return Err(invalid("a v1 info hash is required"));
            }
            for (key, value) in uri.query_pairs() {
                if key == "tr" {
                    let url = Url::parse(&value).map_err(library)?;
                    if value.len() > 2048
                        || !matches!(url.scheme(), "http" | "https" | "udp")
                        || !url.username().is_empty()
                        || url.password().is_some()
                    {
                        return Err(invalid("unsafe magnet tracker URL"));
                    }
                }
            }
            if uri.scheme() != "magnet"
                || uri.query_pairs().filter(|(key, _)| key == "tr").count() > 16
                || !uri
                    .query_pairs()
                    .any(|(key, value)| key == "xt" && value.starts_with("urn:btih:"))
            {
                return Err(invalid("expected a v1 magnet with at most 16 trackers"));
            }
            Ok((Some(magnet.clone()), vec![]))
        }
        (None, Some(encoded)) => {
            if encoded.len() > 700000 {
                return Err(invalid("metainfo exceeds size limit"));
            }
            let bytes = STANDARD.decode(encoded).map_err(library)?;
            metainfo::validate(&bytes)?;
            Ok((None, bytes))
        }
        _ => Err(invalid("supply exactly one magnet or metainfo source")),
    }
}
async fn resolve(session: &Arc<Session>, magnet: &str, options: &Options) -> EngineResult<Vec<u8>> {
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        session.add_torrent(
            AddTorrent::from_url(magnet),
            Some(AddTorrentOptions {
                list_only: true,
                initial_peers: Some(
                    options
                        .initial_peers
                        .iter()
                        .map(|p| p.parse())
                        .collect::<Result<_, _>>()
                        .map_err(library)?,
                ),
                disable_pex: !options.privacy.pex,
                ..Default::default()
            }),
        ),
    )
    .await
    .map_err(|_| invalid("magnet metadata resolution timed out"))?
    .map_err(library)?;
    match result {
        AddTorrentResponse::ListOnly(value) => Ok(value.torrent_bytes.to_vec()),
        _ => Err(invalid("unexpected metadata resolution response")),
    }
}
struct Gate {
    running: std::sync::Weak<Running>,
    down: Arc<TrafficController>,
    up: Arc<TrafficController>,
}
impl librqbit::limits::PayloadGate for Gate {
    fn admit(
        &self,
        upload: bool,
        bytes: usize,
    ) -> futures::future::BoxFuture<'_, librqbit::Result<()>> {
        Box::pin(async move {
            async move {
                let running = self
                    .running
                    .upgrade()
                    .ok_or_else(|| invalid("torrent stopped"))?;
                let data = &running.data;
                let options = &data.options;
                let mut remaining = bytes;
                while remaining > 0 {
                    if data.stopped.load(Ordering::Acquire) {
                        return Err(invalid("torrent stopped"));
                    }
                    if upload && running.finish_then_stop.load(Ordering::Acquire) {
                        return Err(invalid("sharing stopped for shutdown"));
                    }
                    if upload && data.complete.load(Ordering::Acquire) {
                        let elapsed = running
                            .finished
                            .lock()
                            .map_err(|_| invalid("seed clock lock failed"))?
                            .map(|s| s.elapsed())
                            .unwrap_or_default();
                        if options
                            .seed_seconds
                            .is_some_and(|s| elapsed >= Duration::from_secs(s))
                        {
                            return Err(invalid("seeding time limit reached"));
                        }
                    }
                    let controller = if upload { &self.up } else { &self.down };
                    let cap = if upload {
                        options.upload_bps
                    } else {
                        options.download_bps
                    };
                    let allowed = controller.take_limited(
                        data.id,
                        remaining,
                        cap,
                        Instant::now().into(),
                        Utc::now(),
                    )?;
                    remaining -= allowed;
                    if remaining > 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
                if upload {
                    let selected_bytes = data.selected_bytes()?;
                    let quota = options
                        .seed_ratio
                        .map(|ratio| (selected_bytes as f64 * ratio).floor() as u64);
                    if !data
                        .store
                        .reserve_torrent_upload(data.id, bytes as u64, quota)
                        .await?
                    {
                        data.quota_exhausted.store(true, Ordering::Release);
                        return Err(invalid("upload ratio budget exhausted"));
                    }
                }
                Ok(())
            }
            .await
            .map_err(|error: EngineError| librqbit::Error::Embedding(error.into()))
        })
    }
}
impl TorrentService for Adapter {
    fn inspect_torrent(&self, input: Input) -> Reply<'_, Metadata> {
        Box::pin(async move {
            let (magnet, mut raw) = source(&input)?;
            if raw.is_empty() {
                let _permit = self
                    .previews
                    .try_acquire()
                    .map_err(|_| invalid("two metadata previews are already resolving"))?;
                let cancel = CancellationToken::new();
                let _cancel_on_drop = CancelOnDrop(cancel.clone());
                let session = session(&self.root, &input.options, cancel.clone()).await?;
                let result = resolve(
                    &session,
                    magnet.as_deref().ok_or_else(|| invalid("missing magnet"))?,
                    &input.options,
                )
                .await;
                cancel.cancel();
                session.stop().await;
                raw = result?;
            }
            let mut meta = metainfo::validate(&raw)?.metadata;
            if let Some(files) = input.options.files {
                if files.iter().any(|i| *i as usize >= meta.files.len()) {
                    return Err(invalid("selected file index is out of bounds"));
                }
                for file in &mut meta.files {
                    file.selected = files.contains(&file.index);
                }
            }
            Ok(meta)
        })
    }
    fn add_torrent(&self, input: Input) -> Reply<'_, String> {
        Box::pin(async move {
            let (magnet, raw) = source(&input)?;
            let id = JobId::new_v4();
            let destination = input
                .destination
                .map(PathBuf::from)
                .unwrap_or_else(|| self.destination.clone());
            if !destination.is_absolute() {
                return Err(invalid("torrent destination must be absolute"));
            }
            let name = if raw.is_empty() {
                "torrent".to_owned()
            } else {
                metainfo::validate(&raw)?.metadata.name
            };
            let now = Utc::now();
            let job = JobRecord {
                protocol: "torrent".into(),
                capture_id: None,
                requires_session: false,
                use_server_filename: false,
                verified_sha256: None,
                id,
                url: Url::parse(magnet.as_deref().unwrap_or("torrent:metainfo"))
                    .map_err(library)?,
                output_path: destination.join(format!("{name}-{id}")),
                partial_path: self.root.join(id.to_string()),
                total_size: None,
                remote_identity: RemoteIdentity::empty(),
                state: JobState::Queued,
                expected_hash: None,
                conflict_strategy: ConflictStrategy::Fail,
                headers: vec![],
                priority: 0,
                bytes_downloaded: 0,
                created_at: now,
                updated_at: now,
                retry_count: 0,
                error_message: None,
            };
            let descriptor = Descriptor {
                magnet,
                options: input.options,
                uploaded_bytes: 0,
                receipts: vec![],
            };
            self.store
                .create_torrent(
                    &job,
                    &serde_json::to_string(&descriptor).map_err(library)?,
                    &raw,
                )
                .await?;
            Ok(id.to_string())
        })
    }
    fn torrent(&self, id: String) -> Reply<'_, Snapshot> {
        Box::pin(async move {
            let id = JobId::parse_str(&id).map_err(library)?;
            let job = self.record(id).await?;
            let (descriptor, raw) = self.descriptor(id).await?;
            let metadata = if raw.is_empty() {
                None
            } else {
                let mut meta = metainfo::validate(&raw)?.metadata;
                for file in &mut meta.files {
                    file.selected = descriptor
                        .options
                        .files
                        .as_ref()
                        .is_none_or(|v| v.contains(&file.index));
                }
                Some(meta)
            };
            let verified = self
                .store
                .torrent_pieces(id, metadata.as_ref().map_or(0, |m| m.pieces as usize))
                .await?;
            let active = self.active(id)?;
            let handle = active
                .as_ref()
                .and_then(|r| r.handle.lock().ok().and_then(|h| h.clone()));
            let peers = handle
                .as_ref()
                .and_then(|h| h.live())
                .map(|live| {
                    serde_json::to_string(&live.per_peer_stats_snapshot(Default::default()))
                        .unwrap_or_default()
                })
                .unwrap_or_else(|| "{}".into());
            let activity = match job.state {
                JobState::Completed if self.is_seeding(id)? => "seeding",
                JobState::Completed | JobState::Cancelled => "stopped",
                JobState::Paused => "paused",
                JobState::Probing | JobState::Queued => "resolving",
                JobState::Downloading => "downloading",
                JobState::Verifying => "publishing",
                JobState::Failed => "failed",
            };
            Ok(Snapshot {
                id: id.to_string(),
                activity: activity.into(),
                tracker_status: if metadata.as_ref().is_some_and(|m| !m.trackers.is_empty()) {
                    "configured; announce results unavailable"
                } else {
                    "no trackers configured"
                }
                .into(),
                metadata,
                verified,
                peers,
                error: job.error_message,
                uploaded_bytes: handle
                    .map_or(descriptor.uploaded_bytes, |h| h.stats().uploaded_bytes),
                options: descriptor.options,
            })
        })
    }
}
impl DownloadService for Adapter {
    fn add(&self, _: DownloadRequest) -> Reply<'_, JobId> {
        Box::pin(async { Err(invalid("use the typed torrent input")) })
    }
    fn pause(&self, id: JobId) -> Reply<'_, ()> {
        Box::pin(async move { self.stop(id, JobState::Paused).await })
    }
    fn cancel(&self, id: JobId) -> Reply<'_, ()> {
        Box::pin(async move {
            self.stop(id, JobState::Cancelled).await?;
            let job = self.record(id).await?;
            if job.partial_path != self.root.join(id.to_string()) {
                return Err(invalid("unsafe private data location"));
            }
            let owned = self
                .lease
                .lock()
                .map_err(|_| invalid("lease lock failed"))?
                .is_some();
            let _lease = if owned {
                None
            } else {
                match SqliteStore::acquire_engine(self.store.database_path()) {
                    Ok(lease) => Some(lease),
                    Err(_) => return Ok(()),
                }
            };
            if job.partial_path.exists() {
                tokio::fs::remove_dir_all(&job.partial_path)
                    .await
                    .map_err(library)?;
            }
            self.store.replace_torrent_pieces(id, vec![]).await?;
            Ok(())
        })
    }
    fn resume(&self, id: JobId) -> Reply<'_, ()> {
        Box::pin(async move {
            if self.active(id)?.is_some() {
                return Err(invalid(
                    "torrent is still stopping; try again after it quiesces",
                ));
            }
            let job = self.record(id).await?;
            if job.state == JobState::Completed {
                return Err(invalid(
                    "completed torrent is stopped; re-add with explicit seeding options to share",
                ));
            }
            self.state(id, JobState::Queued).await
        })
    }
    fn list(&self, filter: Option<JobState>) -> Reply<'_, Vec<JobRecord>> {
        Box::pin(async move {
            Ok(self
                .store
                .list_jobs(filter)
                .await?
                .into_iter()
                .filter(|j| j.protocol == "torrent")
                .collect())
        })
    }
    fn status(&self, id: JobId) -> Reply<'_, JobRecord> {
        Box::pin(async move { self.record(id).await })
    }
    fn subscribe(&self) -> Reply<'_, broadcast::Receiver<EngineEvent>> {
        Box::pin(async move { Ok(self.events.subscribe()) })
    }
}

async fn barrier(data: Arc<Data>) -> EngineResult<()> {
    tokio::task::spawn_blocking(move || {
        let _guard = data
            .io_gate
            .write()
            .map_err(|_| invalid("IO barrier failed"))?;
        Ok::<_, EngineError>(())
    })
    .await
    .map_err(library)?
}
async fn quiesce(running: &Running) -> EngineResult<()> {
    running.data.stopped.store(true, Ordering::Release);
    running.cancel.cancel();
    if let Some(session) = running.session.get() {
        session.stop().await;
    }
    barrier(running.data.clone()).await?;
    running
        .handle
        .lock()
        .map_err(|_| invalid("torrent handle lock failed"))?
        .take();
    Ok(())
}
struct ExecutionGuard {
    running: Arc<Running>,
    registry: Arc<Mutex<HashMap<JobId, Arc<Running>>>>,
    down: Arc<TrafficController>,
    up: Arc<TrafficController>,
    lease: Option<Arc<okytes_persistence::EngineLease>>,
    armed: bool,
}
impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.running.data.stopped.store(true, Ordering::Release);
        self.running.cancel.cancel();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let running = self.running.clone();
            let registry = self.registry.clone();
            let down = self.down.clone();
            let up = self.up.clone();
            let lease = self.lease.clone();
            runtime.spawn(async move {
                let _lease = lease;
                let _ = quiesce(&running).await;
                let _ = cleanup_cancelled(&running.data).await;
                let _ = running.data.store.torrent_idle(running.data.id).await;
                if let Ok(mut registry) = registry.lock() {
                    registry.remove(&running.data.id);
                }
                down.release(running.data.id);
                up.release(running.data.id);
                running.quiesced.store(true, Ordering::Release);
                running.done.notify_waiters();
            });
        }
    }
}

async fn watch_control(store: &SqliteStore, id: JobId) -> EngineResult<()> {
    loop {
        if store.torrent_stop_requested(id).await? {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
async fn cleanup_cancelled(data: &Data) -> EngineResult<()> {
    if data
        .store
        .load_job(data.id)
        .await?
        .is_some_and(|job| job.state == JobState::Cancelled)
    {
        if data.root.exists() {
            tokio::fs::remove_dir_all(&data.root)
                .await
                .map_err(library)?;
        }
        data.store.replace_torrent_pieces(data.id, vec![]).await?;
    }
    Ok(())
}
