//! Stable Okytes SDK facade. Embedded owners and remote daemon clients are
//! deliberately different types so one process cannot accidentally open the
//! daemon's writable store while intending to attach to it.

use okytes_core::desktop::{Action, DesktopService, FileCredentials, Job, Submit};
use okytes_core::error::EngineError;
use okytes_desktop_service::Session;
use std::{path::PathBuf, sync::Arc, time::Duration};

pub use okytes_core::desktop::{AddInput, Settings, Snapshot};
pub use okytes_core::service::EngineEvent as TransferEvent;
pub use okytes_daemon_api::contract::{Capabilities, Connection, PeerAction, PeerDecision, PeerRequest, PeerSend};
pub use okytes_daemon_api::Client as DaemonClient;
pub use okytes_torrent_types::{Input as TorrentRequest, Metadata as TorrentMetadata};

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub state_directory: PathBuf,
    pub destination: PathBuf,
}

impl EngineConfig {
    pub fn new(state_directory: impl Into<PathBuf>, destination: impl Into<PathBuf>) -> Self {
        Self { state_directory: state_directory.into(), destination: destination.into() }
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
    pub fn destination(mut self, path: impl Into<PathBuf>) -> Self { self.destination = Some(path.into()); self }
    pub fn filename(mut self, value: impl Into<String>) -> Self { self.filename = Some(value.into()); self }
    pub fn expected_sha256(mut self, value: impl Into<String>) -> Self { self.expected_sha256 = Some(value.into()); self }
    pub fn credentials(mut self, value: FileCredentials) -> Self { self.credentials = Some(value); self }
}

#[derive(Debug, Clone, Copy)]
pub enum ConflictPolicy { Rename, Fail, Overwrite }

impl ConflictPolicy {
    fn as_str(self) -> &'static str {
        match self { Self::Rename => "rename", Self::Fail => "fail", Self::Overwrite => "overwrite" }
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
        EngineError::Config(message) if message.contains("lease") || message.contains("owner") => Error::StateAlreadyOwned,
        EngineError::Config(message) => Error::InvalidConfiguration(message),
        other => Error::Transfer(other.to_string()),
    }
}

pub struct EmbeddedEngine {
    session: Arc<Session>,
}

impl EmbeddedEngine {
    pub async fn open(config: EngineConfig) -> Result<Self, Error> {
        if !config.state_directory.is_absolute() || !config.destination.is_absolute() {
            return Err(Error::InvalidConfiguration("state and destination paths must be absolute".into()));
        }
        std::fs::create_dir_all(&config.state_directory).map_err(|error| Error::InvalidConfiguration(error.to_string()))?;
        let session = Session::open(&config.state_directory.join("okytes.db"), &config.destination)
            .await
            .map_err(map)?;
        Ok(Self { session })
    }

    pub async fn submit(&self, request: DownloadRequest) -> Result<TransferHandle, Error> {
        let id = self.session.submit(Submit {
            credentials: request.credentials,
            url: request.url,
            destination: request.destination.map(|path| path.to_string_lossy().into_owned()),
            filename: request.filename,
            sha256: request.expected_sha256,
            conflict: request.conflict.as_str().into(),
        }).await.map_err(map)?;
        Ok(TransferHandle { id, service: self.session.clone() })
    }

    pub async fn list(&self, offset: u32) -> Result<Snapshot, Error> { self.session.list(offset).await.map_err(map) }
    pub async fn get(&self, id: &str) -> Result<Job, Error> { self.session.get(id.to_owned()).await.map_err(map) }
    pub async fn subscribe(&self) -> Result<tokio::sync::broadcast::Receiver<TransferEvent>, Error> { self.session.subscribe().await.map_err(map) }
    pub async fn shutdown(&self) -> Result<(), Error> { self.session.shutdown().await.map_err(map) }
}

#[derive(Clone)]
pub struct TransferHandle {
    id: String,
    service: Arc<Session>,
}

impl TransferHandle {
    pub fn id(&self) -> &str { &self.id }
    pub async fn snapshot(&self) -> Result<Job, Error> { self.service.get(self.id.clone()).await.map_err(map) }
    pub async fn pause(&self) -> Result<(), Error> { self.control(Action::Pause).await }
    pub async fn resume(&self) -> Result<(), Error> { self.control(Action::Resume).await }
    pub async fn cancel(&self) -> Result<(), Error> { self.control(Action::Cancel).await }
    pub async fn retry(&self) -> Result<(), Error> { self.control(Action::Restart).await }
    pub async fn priority(&self, priority: i32) -> Result<(), Error> { self.control(Action::Priority(priority)).await }
    async fn control(&self, action: Action) -> Result<(), Error> { self.service.control(self.id.clone(), action).await.map_err(map) }
    pub async fn wait(&self) -> Result<Job, Error> {
        loop {
            let job = self.snapshot().await?;
            if matches!(job.state.as_str(), "completed" | "failed" | "cancelled") { return Ok(job); }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

pub mod prelude {
    pub use crate::{ConflictPolicy, DaemonClient, DownloadRequest, EmbeddedEngine, EngineConfig, Error, TransferHandle};
}

