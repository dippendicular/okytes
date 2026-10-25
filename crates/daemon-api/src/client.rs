//! Native backend. Redirects cannot carry bearer tokens to another endpoint.
use crate::contract::*;
use futures::StreamExt;
use okytes_core::{
    desktop::{DesktopService, Reply},
    error::{EngineError, EngineResult},
    service::EngineEvent,
};
use reqwest::{Method, Url};
use serde::{de::DeserializeOwned, Serialize};
use std::{path::Path, time::Duration};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct DaemonError {
    pub code: String,
    pub category: ErrorCategory,
    pub retryable: bool,
    pub status: Option<u16>,
    pub message: String,
}

impl DaemonError {
    fn transport(message: &str, retryable: bool) -> Self {
        Self {
            code: "connection_failed".into(),
            category: ErrorCategory::Unavailable,
            retryable,
            status: None,
            message: message.into(),
        }
    }
}

/// GUI-independent daemon backend. Secret fields deliberately omit Debug.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    token: String,
}
fn invalid() -> EngineError {
    EngineError::Config("daemon request failed; check credentials, scope and connection".into())
}
impl Client {
    /// Negotiate only capabilities actually advertised by the connected daemon.
    pub async fn capabilities(&self) -> EngineResult<Capabilities> {
        self.request(Method::GET, "v1/capabilities", None::<&()>)
            .await
    }

    pub async fn peer_requests(&self, offset: u32) -> EngineResult<Vec<PeerRequest>> {
        self.request(
            Method::GET,
            &format!("v1/peers/requests?offset={offset}"),
            None::<&()>,
        )
        .await
    }

    pub async fn peer_send(&self, input: PeerSend) -> EngineResult<String> {
        self.request(Method::POST, "v1/peers/send", Some(&input))
            .await
    }

    pub async fn peer_decide(
        &self,
        id: uuid::Uuid,
        input: PeerDecision,
    ) -> EngineResult<PeerRequest> {
        self.request(
            Method::POST,
            &format!("v1/peers/requests/{id}/decision"),
            Some(&input),
        )
        .await
    }
    /// Ask the local owner for an explicitly approved one-use browser code.
    pub async fn quick_pair(&self) -> EngineResult<QuickPairing> {
        if self.base.scheme() != "http"
            || !self.base.host_str().is_some_and(|host| {
                host.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
            })
        {
            return Err(invalid());
        }
        let pairing: QuickPairing = self
            .request(Method::POST, "v1/quick/pair", Some(&serde_json::json!({})))
            .await?;
        if pairing.url != self.base.join("quick").map_err(|_| invalid())?.as_str()
            || pairing.code.len() != 64
            || !pairing.code.bytes().all(|value| value.is_ascii_hexdigit())
        {
            return Err(invalid());
        }
        Ok(pairing)
    }
    /// Plaintext endpoints must use an explicit loopback address. HTTPS retains
    /// normal certificate verification; unsafe URLs and token redirects fail.
    pub fn new(connection: Connection) -> EngineResult<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let base = Url::parse(&connection.url).map_err(|_| invalid())?;
        let local = base.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !matches!(base.scheme(), "http" | "https")
            || (base.scheme() == "http" && !local)
            || !base.username().is_empty()
            || base.password().is_some()
            || base.path() != "/"
            || base.query().is_some()
            || base.fragment().is_some()
            || connection.token.len() != 101
        {
            return Err(invalid());
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| invalid())?;
        Ok(Self {
            http,
            base,
            token: connection.token,
        })
    }
    /// Only trusted process environment selects an alternate backend. Incomplete
    /// configuration errors rather than silently taking local engine ownership.
    pub fn from_environment() -> EngineResult<Option<Self>> {
        match (
            std::env::var("OKYTES_DAEMON_URL").ok(),
            std::env::var_os("OKYTES_DAEMON_TOKEN_FILE"),
        ) {
            (None, None) => Ok(None),
            (Some(url), Some(file)) => Self::from_file(url, Path::new(&file)).map(Some),
            _ => Err(invalid()),
        }
    }
    /// Read a protected credential file; no secret appears in error strings.
    pub fn from_file(url: String, file: &Path) -> EngineResult<Self> {
        let bytes = std::fs::read(file).map_err(|_| invalid())?;
        if bytes.len() > 4096 {
            return Err(invalid());
        }
        let token = String::from_utf8(bytes)
            .map_err(|_| invalid())?
            .trim()
            .to_owned();
        Self::new(Connection { url, token })
    }
    /// Make a typed, size-bounded authenticated v1 request.
    pub async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> EngineResult<T> {
        self.request_sdk(method, path, body).await.map_err(|error| {
            EngineError::Config(format!("daemon {}: {}", error.code, error.message))
        })
    }

    /// Make a request while preserving the daemon's stable error code,
    /// category, retryability and HTTP status for SDK consumers.
    pub async fn request_sdk<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T, DaemonError> {
        if !path.starts_with("v1/") {
            return Err(DaemonError {
                code: "invalid_path".into(),
                category: ErrorCategory::Validation,
                retryable: false,
                status: None,
                message: "Daemon SDK paths must use the v1 contract.".into(),
            });
        }
        let mut request = self
            .http
            .request(
                method,
                self.base
                    .join(path)
                    .map_err(|_| DaemonError::transport("The daemon path is invalid.", false))?,
            )
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(body);
        }
        self.request_detailed(request).await
    }

    /// Submit a retry-safe creation request with a caller-owned key. Keep the
    /// same key when retrying an ambiguous network failure.
    pub async fn request_idempotent<T: DeserializeOwned>(
        &self,
        path: &str,
        key: &str,
        body: &impl Serialize,
    ) -> Result<T, DaemonError> {
        if !path.starts_with("v1/")
            || key.is_empty()
            || key.len() > 128
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(DaemonError {
                code: "invalid_idempotency_key".into(),
                category: ErrorCategory::Validation,
                retryable: false,
                status: None,
                message: "Use a 1..128 character alphanumeric idempotency key.".into(),
            });
        }
        let request = self
            .http
            .post(
                self.base
                    .join(path)
                    .map_err(|_| DaemonError::transport("The daemon path is invalid.", false))?,
            )
            .bearer_auth(&self.token)
            .header("Idempotency-Key", key)
            .json(body);
        self.request_detailed(request).await
    }

    pub async fn submit_idempotent(
        &self,
        key: &str,
        input: &Submit,
    ) -> Result<String, DaemonError> {
        self.request_idempotent("v1/submit", key, input).await
    }

    pub async fn add_torrent_idempotent(
        &self,
        key: &str,
        input: &okytes_torrent_types::Input,
    ) -> Result<String, DaemonError> {
        self.request_idempotent("v1/torrents", key, input).await
    }

    async fn request_detailed<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, DaemonError> {
        let response = request
            .send()
            .await
            .map_err(|_| DaemonError::transport("The daemon could not be reached.", true))?;
        let status = response.status();
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| {
                DaemonError::transport("The daemon response ended unexpectedly.", true)
            })?;
            if bytes.len() + chunk.len() > 16 * 1024 * 1024 {
                return Err(DaemonError::transport(
                    "The daemon response exceeded the client limit.",
                    false,
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            let body = serde_json::from_slice::<ApiError>(&bytes).unwrap_or(ApiError {
                error: "request_failed".into(),
                category: ErrorCategory::Internal,
                retryable: status.is_server_error(),
                message: format!("Daemon request failed ({status})."),
            });
            return Err(DaemonError {
                code: body.error,
                category: body.category,
                retryable: body.retryable,
                status: Some(status.as_u16()),
                message: body.message,
            });
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| DaemonError::transport("The daemon returned an invalid response.", false))
    }
    /// Preserve browser request IDs and ephemeral headers through the daemon API.
    #[cfg(feature = "browser-capture")]
    pub async fn capture(
        &self,
        request: okytes_browser_bridge::protocol::Request,
    ) -> EngineResult<okytes_browser_bridge::protocol::Response> {
        request.validate().map_err(|_| invalid())?;
        self.request(Method::POST, "v1/capture", Some(&request))
            .await
    }
    async fn events(&self, cursor: Option<&str>) -> EngineResult<reqwest::Response> {
        let mut request = self
            .http
            .get(self.base.join("v1/events").map_err(|_| invalid())?)
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(3600));
        if let Some(cursor) = cursor {
            request = request.header("Last-Event-ID", cursor);
        }
        let response = request.send().await.map_err(|_| invalid())?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(invalid())
        }
    }

    /// Subscribe to bounded contract updates. `Resync` is authoritative: the
    /// caller must replace cached jobs with a fresh snapshot. SSE cursors are
    /// retained across reconnects and backoff is capped at ten seconds.
    pub async fn updates(&self) -> EngineResult<tokio::sync::mpsc::Receiver<Update>> {
        let first = self.events(None).await?;
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let client = self.clone();
        tokio::spawn(async move {
            let mut response = Some(first);
            let mut cursor: Option<String> = None;
            let mut delay = Duration::from_millis(250);
            loop {
                let Some(current) = response.take() else {
                    return;
                };
                let mut stream = current.bytes_stream();
                let mut pending = Vec::new();
                while let Some(Ok(chunk)) = stream.next().await {
                    if tx.is_closed() || pending.len() + chunk.len() > 131_072 {
                        return;
                    }
                    pending.extend_from_slice(&chunk);
                    while let Some(end) = pending.windows(2).position(|pair| pair == b"\n\n") {
                        let frame: Vec<_> = pending.drain(..end + 2).collect();
                        if let Some(line) = frame
                            .split(|byte| *byte == b'\n')
                            .find(|line| line.starts_with(b"id:"))
                        {
                            cursor = std::str::from_utf8(&line[3..])
                                .ok()
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .map(str::to_owned);
                        }
                        let data = frame.split(|byte| *byte == b'\n').find_map(|line| {
                            line.strip_prefix(b"data: ")
                                .or_else(|| line.strip_prefix(b"data:"))
                        });
                        if let Some(Ok(update)) = data.map(serde_json::from_slice::<Update>) {
                            if tx.send(update).await.is_err() {
                                return;
                            }
                        }
                    }
                }
                if tx.is_closed() {
                    return;
                }
                let _ = tx.send(Update::Resync).await;
                loop {
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(10));
                    if tx.is_closed() {
                        return;
                    }
                    if let Ok(next) = client.events(cursor.as_deref()).await {
                        response = Some(next);
                        delay = Duration::from_millis(250);
                        break;
                    }
                }
            }
        });
        Ok(rx)
    }
}
impl DesktopService for Client {
    fn inspect_media(&self, url: String) -> Reply<'_, Vec<okytes_core::desktop::MediaChoice>> {
        Box::pin(async move {
            self.request(Method::POST, "v1/media/inspect", Some(&url))
                .await
        })
    }
    fn authorize_file(
        &self,
        id: String,
        credentials: okytes_core::desktop::FileCredentials,
    ) -> Reply<'_, ()> {
        Box::pin(async move {
            let id: uuid::Uuid = id.parse().map_err(|_| invalid())?;
            self.request(
                Method::POST,
                &format!("v1/jobs/{id}/credentials"),
                Some(&credentials),
            )
            .await
        })
    }
    fn submit(&self, input: Submit) -> Reply<'_, String> {
        Box::pin(async move { self.request(Method::POST, "v1/submit", Some(&input)).await })
    }
    fn pause_all(&self) -> Reply<'_, ()> {
        Box::pin(async move { self.request(Method::POST, "v1/pause-all", Some(&())).await })
    }
    fn add(&self, input: AddInput) -> Reply<'_, Vec<String>> {
        Box::pin(async move { self.request(Method::POST, "v1/jobs", Some(&input)).await })
    }
    fn list(&self, offset: u32) -> Reply<'_, Snapshot> {
        Box::pin(async move {
            self.request(
                Method::GET,
                &format!("v1/jobs?offset={offset}"),
                None::<&()>,
            )
            .await
        })
    }
    fn get(&self, id: String) -> Reply<'_, Job> {
        Box::pin(async move {
            uuid::Uuid::parse_str(&id).map_err(|_| invalid())?;
            self.request(Method::GET, &format!("v1/jobs/{id}"), None::<&()>)
                .await
        })
    }
    fn control(&self, id: String, action: Action) -> Reply<'_, ()> {
        Box::pin(async move {
            uuid::Uuid::parse_str(&id).map_err(|_| invalid())?;
            self.request(
                Method::POST,
                &format!("v1/jobs/{id}/control"),
                Some(&action),
            )
            .await
        })
    }
    fn settings(&self) -> Reply<'_, Settings> {
        Box::pin(async move { self.request(Method::GET, "v1/settings", None::<&()>).await })
    }
    fn set_settings(&self, settings: Settings) -> Reply<'_, ()> {
        Box::pin(async move {
            self.request(Method::PUT, "v1/settings", Some(&settings))
                .await
        })
    }
    fn subscribe(&self) -> Reply<'_, tokio::sync::broadcast::Receiver<EngineEvent>> {
        Box::pin(async move {
            let response = self.events(None).await?;
            let (tx, rx) = tokio::sync::broadcast::channel(64);
            let client = self.clone();
            tokio::spawn(async move {
                let mut response = response;
                let mut cursor: Option<String> = None;
                loop {
                    let mut stream = response.bytes_stream();
                    let mut pending = Vec::new();
                    while let Some(Ok(chunk)) = stream.next().await {
                        if tx.receiver_count() == 0 || pending.len() + chunk.len() > 131072 {
                            return;
                        }
                        pending.extend_from_slice(&chunk);
                        while let Some(end) = pending.windows(2).position(|pair| pair == b"\n\n") {
                            let frame: Vec<_> = pending.drain(..end + 2).collect();
                            if let Some(line) = frame
                                .split(|byte| *byte == b'\n')
                                .find(|line| line.starts_with(b"id:"))
                            {
                                cursor = std::str::from_utf8(&line[3..])
                                    .ok()
                                    .map(str::trim)
                                    .filter(|value| !value.is_empty())
                                    .map(str::to_owned);
                            }
                            if !frame.starts_with(b"data:") {
                                continue;
                            }
                            let data = frame
                                .strip_prefix(b"data: ")
                                .or_else(|| frame.strip_prefix(b"data:"));
                            if let Some(data) = data {
                                if let Ok(Update::Changed(event)) =
                                    serde_json::from_slice::<Update>(data)
                                {
                                    let _ = tx.send(event);
                                }
                            }
                        }
                    }
                    if tx.receiver_count() == 0 {
                        return;
                    }
                    loop {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        if tx.receiver_count() == 0 {
                            return;
                        }
                        if let Ok(next) = client.events(cursor.as_deref()).await {
                            response = next;
                            break;
                        }
                    }
                }
            });
            Ok(rx)
        })
    }
}

impl okytes_torrent_types::TorrentService for Client {
    fn inspect_torrent(
        &self,
        input: okytes_torrent_types::Input,
    ) -> Reply<'_, okytes_torrent_types::Metadata> {
        Box::pin(async move {
            self.request(Method::POST, "v1/torrents/inspect", Some(&input))
                .await
        })
    }
    fn add_torrent(&self, input: okytes_torrent_types::Input) -> Reply<'_, String> {
        Box::pin(async move {
            self.request(Method::POST, "v1/torrents", Some(&input))
                .await
        })
    }
    fn torrent(&self, id: String) -> Reply<'_, okytes_torrent_types::Snapshot> {
        Box::pin(async move {
            let id = uuid::Uuid::parse_str(&id).map_err(|_| invalid())?;
            self.request(Method::GET, &format!("v1/torrents/{id}"), None::<&()>)
                .await
        })
    }
}
