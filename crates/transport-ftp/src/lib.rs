//! FTP/explicit FTPS and SFTP over the engine's durable byte-range interface.
//! Credentials are request-local memory only. Size and modification time are
//! required on every fetch; unknown or changed SSH host keys fail closed.
use bytes::Bytes;
pub mod router;
use okytes_core::{
    error::TransportError,
    transport::{ContentRange, FetchResult, ProbeResult, Transport},
    types::{EngineConfig, RemoteIdentity},
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use suppaftp::tokio::{AsyncRustlsConnector, AsyncRustlsFtpStream};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::{OwnedSemaphorePermit, Semaphore},
};
use url::Url;
type Ftp = AsyncRustlsFtpStream;
type Future<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, TransportError>> + Send + 'a>>;
fn error(url: &Url, detail: impl std::fmt::Display) -> TransportError {
    TransportError::Request {
        url: Box::new(url.clone()),
        detail: detail.to_string(),
    }
}
fn credential(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}
fn path(url: &Url) -> Result<String, TransportError> {
    let value = percent_encoding::percent_decode_str(url.path())
        .decode_utf8()
        .map_err(|_| error(url, "invalid UTF-8 remote path"))?
        .into_owned();
    if value.bytes().any(|b| b < 32 || b == 127) {
        return Err(error(url, "control characters in remote path"));
    }
    Ok(value)
}
struct Permit {
    _global: OwnedSemaphorePermit,
    _host: OwnedSemaphorePermit,
}
/// Certificate-validating, bounded file transport. Idle FTP controls are reused.
pub struct FileTransport {
    global: Arc<Semaphore>,
    hosts: Mutex<HashMap<String, Weak<Semaphore>>>,
    cap: usize,
    timeout: Duration,
    tls: Arc<rustls::ClientConfig>,
    known_hosts: PathBuf,
    idle: Arc<tokio::sync::Mutex<Vec<(String, Ftp, Permit)>>>,
    trust_lock: Arc<Mutex<()>>,
}
impl FileTransport {
    /// Use WebPKI roots and this profile's OpenSSH-format known-hosts store.
    pub fn new(config: &EngineConfig, known_hosts: PathBuf) -> Result<Self, TransportError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Self::with_roots(config, known_hosts, roots)
    }
    /// Supply trusted roots (for enterprise/private certificate authorities).
    /// This never bypasses certificate or hostname verification.
    pub fn with_roots(
        config: &EngineConfig,
        known_hosts: PathBuf,
        roots: rustls::RootCertStore,
    ) -> Result<Self, TransportError> {
        config
            .validate()
            .map_err(|e| TransportError::Configuration(e.to_string()))?;
        Ok(Self {
            global: Arc::new(Semaphore::new(config.max_connections_total)),
            hosts: Mutex::new(HashMap::new()),
            cap: config.max_connections_per_origin,
            timeout: Duration::from_secs(config.read_timeout_secs),
            tls: Arc::new(
                rustls::ClientConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .map_err(|e| TransportError::Configuration(e.to_string()))?
                .with_root_certificates(roots)
                .with_no_client_auth(),
            ),
            known_hosts,
            idle: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            trust_lock: Arc::new(Mutex::new(())),
        })
    }
    async fn permit(&self, url: &Url) -> Result<Permit, TransportError> {
        // Idle controls own permits too. Evict unmatched idle connections before
        // waiting, so small global caps cannot deadlock behind the idle cache.
        self.idle.lock().await.clear();
        if !matches!(url.scheme(), "ftp" | "ftps" | "sftp")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(error(url, "file URL without embedded credentials required"));
        }
        let key = format!(
            "{}:{}:{}",
            url.scheme(),
            url.host_str().unwrap_or_default(),
            url.port()
                .unwrap_or(if url.scheme() == "sftp" { 22 } else { 21 })
        );
        let host = {
            let mut hosts = self
                .hosts
                .lock()
                .map_err(|_| error(url, "host permit lock failed"))?;
            hosts.retain(|_, s| s.strong_count() > 0);
            let s = hosts
                .get(&key)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| Arc::new(Semaphore::new(self.cap)));
            hosts.insert(key, Arc::downgrade(&s));
            s
        };
        Ok(Permit {
            _global: self
                .global
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| error(url, e))?,
            _host: host.acquire_owned().await.map_err(|e| error(url, e))?,
        })
    }
    async fn ftp(
        &self,
        url: &Url,
        headers: &[(String, String)],
    ) -> Result<(String, Ftp, Permit), TransportError> {
        if !url.username().is_empty() || url.password().is_some() {
            return Err(error(url, "embedded credentials are refused"));
        }
        let user = credential(headers, "okytes-user").unwrap_or_else(|| "anonymous".into());
        let password =
            credential(headers, "okytes-password").unwrap_or_else(|| "anonymous@".into());
        let key = format!(
            "{}:{}:{}\0{user}\0{password}",
            url.scheme(),
            url.host_str().unwrap_or_default(),
            url.port().unwrap_or(21)
        );
        let cached = {
            let mut idle = self.idle.lock().await;
            idle.iter().position(|(k, _, _)| k == &key).map(|index| {
                let (_, ftp, permit) = idle.swap_remove(index);
                (ftp, permit)
            })
        };
        if let Some((mut ftp, permit)) = cached {
            if ftp.noop().await.is_ok() {
                return Ok((key, ftp, permit));
            }
        }
        let permit = self.permit(url).await?;
        let mut ftp = Ftp::connect((url.host_str().unwrap_or_default(), url.port().unwrap_or(21)))
            .await
            .map_err(|e| error(url, e))?;
        if url.scheme() == "ftps" {
            ftp = ftp
                .into_secure(
                    AsyncRustlsConnector::from(tokio_rustls::TlsConnector::from(self.tls.clone())),
                    url.host_str().unwrap_or_default(),
                )
                .await
                .map_err(|e| error(url, e))?;
        }
        ftp.login(&user, &password)
            .await
            .map_err(|_| TransportError::Configuration("FTP authentication failed".into()))?;
        ftp.transfer_type(suppaftp::types::FileType::Binary)
            .await
            .map_err(|e| error(url, e))?;
        ftp.set_mode(suppaftp::Mode::ExtendedPassive);
        Ok((key, ftp, permit))
    }
    async fn ftp_identity(ftp: &mut Ftp, url: &Url) -> Result<RemoteIdentity, TransportError> {
        let path = path(url)?;
        let size = ftp.size(&path).await.map_err(|e| error(url, e))?;
        let modified = ftp.mdtm(&path).await.map_err(|_| {
            TransportError::Configuration("FTP MDTM is required for safe resume".into())
        })?;
        Ok(RemoteIdentity {
            etag: None,
            last_modified: Some(modified.to_string()),
            content_length: Some(size as u64),
        })
    }
    async fn ssh(
        &self,
        url: &Url,
        headers: &[(String, String)],
    ) -> Result<
        (
            russh::client::Handle<HostCheck>,
            russh_sftp::client::SftpSession,
        ),
        TransportError,
    > {
        let user = credential(headers, "okytes-user").ok_or_else(|| {
            TransportError::Configuration("SFTP username and password required".into())
        })?;
        let password = credential(headers, "okytes-password").ok_or_else(|| {
            TransportError::Configuration(
                "SFTP password must be supplied again after restart".into(),
            )
        })?;
        let handler = HostCheck {
            host: url.host_str().unwrap_or_default().into(),
            port: url.port().unwrap_or(22),
            path: self.known_hosts.clone(),
            confirmation: credential(headers, "okytes-host-key"),
            lock: self.trust_lock.clone(),
        };
        let config = russh::client::Config {
            inactivity_timeout: Some(self.timeout),
            ..Default::default()
        };
        let mut ssh = russh::client::connect(
            Arc::new(config),
            (url.host_str().unwrap_or_default(), url.port().unwrap_or(22)),
            handler,
        )
        .await
        .map_err(|e| {
            let detail = e.to_string();
            if detail.contains("SFTP host key") {
                TransportError::Configuration(detail)
            } else {
                error(url, detail)
            }
        })?;
        if !ssh
            .authenticate_password(user, password)
            .await
            .map_err(|_| TransportError::Configuration("SFTP authentication failed".into()))?
            .success()
        {
            return Err(TransportError::Configuration(
                "SFTP authentication failed".into(),
            ));
        }
        let channel = ssh
            .channel_open_session()
            .await
            .map_err(|e| error(url, e))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| error(url, e))?;
        let sftp = russh_sftp::client::SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| error(url, e))?;
        Ok((ssh, sftp))
    }
    async fn sftp_identity(
        sftp: &russh_sftp::client::SftpSession,
        url: &Url,
    ) -> Result<RemoteIdentity, TransportError> {
        let stat = sftp.metadata(path(url)?).await.map_err(|e| error(url, e))?;
        Ok(RemoteIdentity {
            etag: None,
            last_modified: Some(
                stat.mtime
                    .ok_or_else(|| error(url, "SFTP modification time required"))?
                    .to_string(),
            ),
            content_length: Some(stat.size.ok_or_else(|| error(url, "SFTP size required"))?),
        })
    }
}
struct HostCheck {
    host: String,
    port: u16,
    path: PathBuf,
    confirmation: Option<String>,
    lock: Arc<Mutex<()>>,
}
impl russh::client::Handler for HostCheck {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let _guard = self.lock.lock().map_err(|_| russh::Error::Inconsistent)?;
        if matches!(key, russh::keys::PublicKeyOrCertificate::Certificate(_)) {
            return Err(russh::Error::Inconsistent);
        }
        let key = key.public_key();
        let recorded =
            russh::keys::known_hosts::known_host_keys_path(&self.host, self.port, &self.path)?;
        if !recorded.is_empty() {
            // Reject changes even when the server switched its key algorithm.
            return if recorded.iter().any(|(_, k)| k == &key) {
                Ok(true)
            } else {
                Err(russh::Error::IO(std::io::Error::other(
                    "SFTP host key changed; connection refused",
                )))
            };
        }
        let fingerprint = key
            .fingerprint(russh::keys::ssh_key::HashAlg::Sha256)
            .to_string();
        if self.confirmation.as_deref() != Some(&fingerprint) {
            return Err(russh::Error::IO(std::io::Error::other(format!("Unknown SFTP host key {fingerprint}. Verify it with the server administrator and explicitly confirm this fingerprint."))));
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        russh::keys::known_hosts::learn_known_hosts_path(&self.host, self.port, &key, &self.path)?;
        Ok(true)
    }
}
impl Transport for FileTransport {
    fn supports(&self, scheme: &str) -> bool {
        matches!(scheme, "ftp" | "ftps" | "sftp")
    }
    fn probe(&self, url: &Url, headers: &[(String, String)]) -> Future<'_, ProbeResult> {
        let url = url.clone();
        let headers = headers.to_vec();
        Box::pin(async move {
            tokio::time::timeout(self.timeout, async {
                let identity = if url.scheme() == "sftp" {
                    let _permit = self.permit(&url).await?;
                    let (_ssh, sftp) = self.ssh(&url, &headers).await?;
                    Self::sftp_identity(&sftp, &url).await?
                } else {
                    let (key, mut ftp, permit) = self.ftp(&url, &headers).await?;
                    let identity = Self::ftp_identity(&mut ftp, &url).await?;
                    let mut idle = self.idle.lock().await;
                    if idle.len() < 16 {
                        idle.push((key, ftp, permit));
                    }
                    identity
                };
                Ok(ProbeResult {
                    checkpoint_ranges: None,
                    final_url: url.clone(),
                    content_length: identity.content_length,
                    accepts_ranges: true,
                    remote_identity: identity,
                    suggested_filename: Some(
                        path(&url)?
                            .rsplit('/')
                            .next()
                            .unwrap_or("download")
                            .to_owned(),
                    ),
                    content_type: None,
                })
            })
            .await
            .map_err(|_| TransportError::Timeout { url: Box::new(url) })?
        })
    }
    fn fetch(
        &self,
        url: &Url,
        headers: &[(String, String)],
        range: Option<(u64, u64)>,
        identity: &RemoteIdentity,
    ) -> Future<'_, FetchResult> {
        let url = url.clone();
        let headers = headers.to_vec();
        let expected = identity.clone();
        let idle = self.idle.clone();
        let timeout = self.timeout;
        let timeout_source = url.clone();
        let future = async move {
            let (actual, body): (_, okytes_core::transport::ByteStream) = if url.scheme() == "sftp"
            {
                let permit = self.permit(&url).await?;
                let (ssh, sftp) = tokio::time::timeout(timeout, self.ssh(&url, &headers))
                    .await
                    .map_err(|_| error(&url, "SFTP connection timed out"))??;
                let actual = Self::sftp_identity(&sftp, &url).await?;
                if !expected.is_compatible(&actual) {
                    return Err(TransportError::Configuration(
                        "remote file size or modification time identity changed; restart required"
                            .into(),
                    ));
                }
                let mut file = sftp.open(path(&url)?).await.map_err(|e| error(&url, e))?;
                let (start, end) = bounds(&url, range, &actual)?;
                file.seek(std::io::SeekFrom::Start(start))
                    .await
                    .map_err(|e| error(&url, e))?;
                let target = url.clone();
                let original = actual.clone();
                let body = async_stream::try_stream! {
                    let _permit=permit;let _ssh=ssh;let mut remaining=if original.content_length==Some(0){0}else{end-start+1};let mut buffer=vec![0u8;32768];
                    while remaining>0 {let count=remaining.min(buffer.len() as u64) as usize;
                        let n=tokio::time::timeout(timeout,file.read(&mut buffer[..count])).await.map_err(|_|error(&target,"SFTP read timed out"))?.map_err(|e|error(&target,e))?;
                        if n==0 {Err(error(&target,"SFTP transfer ended early"))?;}remaining-=n as u64;yield Bytes::copy_from_slice(&buffer[..n]);}
                    if !original.is_compatible(&tokio::time::timeout(timeout,Self::sftp_identity(&sftp,&target)).await.map_err(|_|error(&target,"SFTP identity check timed out"))??){Err(TransportError::Configuration("SFTP source identity changed during transfer".into()))?;}
                };
                (actual, Box::pin(body))
            } else {
                let (key, mut ftp, permit) =
                    tokio::time::timeout(timeout, self.ftp(&url, &headers))
                        .await
                        .map_err(|_| error(&url, "FTP connection timed out"))??;
                let actual = Self::ftp_identity(&mut ftp, &url).await?;
                if !expected.is_compatible(&actual) {
                    return Err(TransportError::Configuration(
                        "remote file size or modification time identity changed; restart required"
                            .into(),
                    ));
                }
                let (start, end) = bounds(&url, range, &actual)?;
                ftp.resume_transfer(
                    usize::try_from(start).map_err(|_| error(&url, "FTP offset too large"))?,
                )
                .await
                .map_err(|e| error(&url, e))?;
                let mut data = ftp
                    .retr_as_stream(path(&url)?)
                    .await
                    .map_err(|e| error(&url, e))?;
                let target = url.clone();
                let original = actual.clone();
                let total = actual.content_length.unwrap_or(0);
                let body = async_stream::try_stream! {
                    let _permit=permit;let mut remaining=if original.content_length==Some(0){0}else{end-start+1};let mut buffer=vec![0u8;32768];
                    while remaining>0 {let count=remaining.min(buffer.len() as u64) as usize;
                        let n=tokio::time::timeout(timeout,data.read(&mut buffer[..count])).await.map_err(|_|error(&target,"FTP read timed out"))?.map_err(|e|error(&target,e))?;
                        if n==0 {Err(error(&target,"FTP transfer ended early"))?;}remaining-=n as u64;yield Bytes::copy_from_slice(&buffer[..n]);}
                    // A bounded range before EOF deliberately drops its control rather
                    // than reusing a connection with an unread completion response.
                    if end+1==total {tokio::time::timeout(timeout,data.finish()).await.map_err(|_|error(&target,"FTP completion timed out"))?.map_err(|e|error(&target,e))?;
                        if !original.is_compatible(&tokio::time::timeout(timeout,FileTransport::ftp_identity(&mut ftp,&target)).await.map_err(|_|error(&target,"FTP identity check timed out"))??){Err(TransportError::Configuration("FTP source identity changed during transfer".into()))?;}
                        let mut cache=idle.lock().await;if cache.len()<16{cache.push((key,ftp,_permit));}}
                };
                (actual, Box::pin(body))
            };
            let (start, end) = bounds(&url, range, &actual)?;
            Ok(FetchResult {
                status: if range.is_some() { 206 } else { 200 },
                content_length: Some(if actual.content_length == Some(0) {
                    0
                } else {
                    end - start + 1
                }),
                content_range: range.map(|_| ContentRange {
                    start,
                    end,
                    total: actual.content_length,
                }),
                remote_identity: actual,
                body,
            })
        };
        Box::pin(async move {
            tokio::time::timeout(timeout, future)
                .await
                .map_err(|_| TransportError::Timeout {
                    url: Box::new(timeout_source),
                })?
        })
    }
}
fn bounds(
    url: &Url,
    range: Option<(u64, u64)>,
    identity: &RemoteIdentity,
) -> Result<(u64, u64), TransportError> {
    let size = identity
        .content_length
        .ok_or_else(|| error(url, "remote size required"))?;
    let bounds = range.unwrap_or((0, size.saturating_sub(1)));
    if size == 0 && range.is_none() {
        return Ok((0, 0));
    }
    if size == 0 || bounds.0 > bounds.1 || bounds.1 >= size {
        return Err(error(url, "empty file or invalid range"));
    }
    Ok(bounds)
}
