//! Stable Okytes SDK facade. Embedded owners and remote daemon clients are
//! deliberately different types so one process cannot accidentally open the
//! daemon's writable store while intending to attach to it.

use okytes_core::desktop::{Action, DesktopService, FileCredentials, Job, Submit};
use okytes_core::error::EngineError;
use okytes_desktop_service::Session;
use okytes_torrent_types::TorrentService;
use std::{path::PathBuf, sync::Arc, time::Duration};

pub use okytes_core::desktop::{AddInput, Settings, Snapshot};
pub use okytes_core::service::EngineEvent as TransferEvent;
pub use okytes_daemon_api::contract::{
    Capabilities, Connection, PeerAction, PeerDecision, PeerRequest, PeerSend,
};
pub use okytes_daemon_api::Client as DaemonClient;
pub use okytes_torrent_types::{Input as TorrentRequest, Metadata as TorrentMetadata};

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub state_directory: PathBuf,
    pub destination: PathBuf,
    pub concurrency: u32,
    pub shutdown_timeout: Duration,
}

impl EngineConfig {
    pub fn new(state_directory: impl Into<PathBuf>, destination: impl Into<PathBuf>) -> Self {
        Self {
            state_directory: state_directory.into(),
            destination: destination.into(),
            concurrency: 4,
            shutdown_timeout: Duration::from_secs(30),
        }
    }
    pub fn concurrency(mut self, value: u32) -> Self {
        self.concurrency = value;
        self
    }
    pub fn shutdown_timeout(mut self, value: Duration) -> Self {
        self.shutdown_timeout = value;
        self
    }
}

#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub url: String,
    pub destination: Option<PathBuf>,
    pub filename: Option<String>,
    pub expected_sha256: Option<String>,
    pub conflict: ConflictPolicy,
    pub credentials: Option<FileCredentials>,
}

impl DownloadRequest {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            destination: None,
            filename: None,
            expected_sha256: None,
            conflict: ConflictPolicy::Rename,
            credentials: None,
        }
    }
    pub fn destination(mut self, path: impl Into<PathBuf>) -> Self {
        self.destination = Some(path.into());
        self
    }
    pub fn filename(mut self, value: impl Into<String>) -> Self {
        self.filename = Some(value.into());
        self
    }
    pub fn expected_sha256(mut self, value: impl Into<String>) -> Self {
        self.expected_sha256 = Some(value.into());
        self
    }
    pub fn credentials(mut self, value: FileCredentials) -> Self {
        self.credentials = Some(value);
        self
    }
    pub fn ftp(
        url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self::new(url).credentials(FileCredentials {
            username: username.into(),
            password: password.into(),
            confirmed_host_key: None,
        })
    }
    pub fn sftp(
        url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
        confirmed_host_key: impl Into<String>,
    ) -> Self {
        Self::new(url).credentials(FileCredentials {
            username: username.into(),
            password: password.into(),
            confirmed_host_key: Some(confirmed_host_key.into()),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct TransferFilter {
    pub state: Option<String>,
    pub protocol: Option<String>,
    pub minimum_priority: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct TransferResult {
    pub id: String,
    pub destination: PathBuf,
    pub protocol: String,
    pub bytes: u64,
    pub expected_sha256: Option<String>,
    pub remote_identity: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum ConflictPolicy {
    Rename,
    Fail,
    Overwrite,
}

impl ConflictPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Rename => "rename",
            Self::Fail => "fail",
            Self::Overwrite => "overwrite",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("state directory is already owned or unavailable")]
    StateAlreadyOwned,
    #[error("invalid SDK configuration: {0}")]
    InvalidConfiguration(String),
    #[error("transfer not found")]
    NotFound,
    #[error("transfer operation failed: {0}")]
    Transfer(String),
}

fn map(error: EngineError) -> Error {
    match error {
        EngineError::JobNotFound(_) => Error::NotFound,
        EngineError::Config(message) if message.contains("lease") || message.contains("owner") => {
            Error::StateAlreadyOwned
        }
        EngineError::Config(message) => Error::InvalidConfiguration(message),
        other => Error::Transfer(other.to_string()),
    }
}

pub struct EmbeddedEngine {
    session: Arc<Session>,
    shutdown_timeout: Duration,
}

impl EmbeddedEngine {
    pub async fn open(config: EngineConfig) -> Result<Self, Error> {
        if !config.state_directory.is_absolute() || !config.destination.is_absolute() {
            return Err(Error::InvalidConfiguration(
                "state and destination paths must be absolute".into(),
            ));
        }
        std::fs::create_dir_all(&config.state_directory)
            .map_err(|error| Error::InvalidConfiguration(error.to_string()))?;
        if !(1..=32).contains(&config.concurrency) || config.shutdown_timeout.is_zero() {
            return Err(Error::InvalidConfiguration(
                "concurrency must be 1..=32 and shutdown timeout must be positive".into(),
            ));
        }
        let session = Session::open(
            &config.state_directory.join("okytes.db"),
            &config.destination,
        )
        .await
        .map_err(map)?;
        let mut settings = session.settings().await.map_err(map)?;
        settings.concurrency = config.concurrency;
        session.set_settings(settings).await.map_err(map)?;
        Ok(Self {
            session,
            shutdown_timeout: config.shutdown_timeout,
        })
    }

    pub async fn submit(&self, request: DownloadRequest) -> Result<TransferHandle, Error> {
        let expected_sha256 = request.expected_sha256.clone();
        let id = self
            .session
            .submit(Submit {
                credentials: request.credentials,
                url: request.url,
                destination: request
                    .destination
                    .map(|path| path.to_string_lossy().into_owned()),
                filename: request.filename,
                sha256: request.expected_sha256,
                conflict: request.conflict.as_str().into(),
            })
            .await
            .map_err(map)?;
        Ok(TransferHandle {
            id,
            service: self.session.clone(),
            expected_sha256,
        })
    }

    pub async fn submit_torrent(&self, request: TorrentRequest) -> Result<TransferHandle, Error> {
        let id = self.session.add_torrent(request).await.map_err(map)?;
        Ok(TransferHandle {
            id,
            service: self.session.clone(),
            expected_sha256: None,
        })
    }
    pub async fn inspect_torrent(&self, request: TorrentRequest) -> Result<TorrentMetadata, Error> {
        self.session.inspect_torrent(request).await.map_err(map)
    }
    pub async fn inspect_media(
        &self,
        url: impl Into<String>,
    ) -> Result<Vec<okytes_core::desktop::MediaChoice>, Error> {
        self.session.inspect_media(url.into()).await.map_err(map)
    }

    pub async fn list(&self, offset: u32) -> Result<Snapshot, Error> {
        self.session.list(offset).await.map_err(map)
    }
    pub async fn filtered(&self, offset: u32, filter: &TransferFilter) -> Result<Vec<Job>, Error> {
        let snapshot = self.list(offset).await?;
        Ok(snapshot
            .jobs
            .into_iter()
            .filter(|job| {
                filter
                    .state
                    .as_ref()
                    .is_none_or(|value| &job.state == value)
                    && filter
                        .protocol
                        .as_ref()
                        .is_none_or(|value| &job.protocol == value)
                    && filter
                        .minimum_priority
                        .is_none_or(|value| job.priority >= value)
            })
            .collect())
    }
    pub async fn get(&self, id: &str) -> Result<Job, Error> {
        self.session.get(id.to_owned()).await.map_err(map)
    }
    pub async fn subscribe(
        &self,
    ) -> Result<tokio::sync::broadcast::Receiver<TransferEvent>, Error> {
        self.session.subscribe().await.map_err(map)
    }
    pub async fn shutdown(&self) -> Result<(), Error> {
        tokio::time::timeout(self.shutdown_timeout, self.session.shutdown()).await
            .map_err(|_| Error::Transfer("graceful shutdown exceeded its configured timeout; durable recovery will run on the next open".into()))?
            .map_err(map)
    }
}

#[derive(Clone)]
pub struct TransferHandle {
    id: String,
    service: Arc<Session>,
    expected_sha256: Option<String>,
}

impl TransferHandle {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub async fn snapshot(&self) -> Result<Job, Error> {
        self.service.get(self.id.clone()).await.map_err(map)
    }
    pub async fn pause(&self) -> Result<(), Error> {
        self.control(Action::Pause).await
    }
    pub async fn resume(&self) -> Result<(), Error> {
        self.control(Action::Resume).await
    }
    pub async fn cancel(&self) -> Result<(), Error> {
        self.control(Action::Cancel).await
    }
    pub async fn retry(&self) -> Result<(), Error> {
        self.control(Action::Restart).await
    }
    pub async fn priority(&self, priority: i32) -> Result<(), Error> {
        self.control(Action::Priority(priority)).await
    }
    async fn control(&self, action: Action) -> Result<(), Error> {
        self.service
            .control(self.id.clone(), action)
            .await
            .map_err(map)
    }
    pub async fn wait(&self) -> Result<TransferResult, Error> {
        loop {
            let job = self.snapshot().await?;
            if job.state == "completed" {
                return Ok(TransferResult {
                    id: job.id,
                    destination: PathBuf::from(job.destination),
                    protocol: job.protocol,
                    bytes: job.bytes as u64,
                    expected_sha256: self.expected_sha256.clone(),
                    remote_identity: job.etag,
                });
            }
            if matches!(job.state.as_str(), "failed" | "cancelled") {
                return Err(Error::Transfer(
                    job.error
                        .unwrap_or_else(|| format!("transfer ended in {}", job.state)),
                ));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

pub mod prelude {
    pub use crate::{
        ConflictPolicy, DaemonClient, DownloadRequest, EmbeddedEngine, EngineConfig, Error,
        TransferFilter, TransferHandle, TransferResult,
    };
}
