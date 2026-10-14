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
        let bytes = okytes_browser_bridge::read_secret(file).map_err(|_| invalid())?;
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
        if !path.starts_with("v1/") {
            return Err(invalid());
        }
        let mut request = self
            .http
            .request(method, self.base.join(path).map_err(|_| invalid())?)
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|_| invalid())?;
        if !response.status().is_success() {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| invalid())?;
            if bytes.len() + chunk.len() > 16 * 1024 * 1024 {
                return Err(invalid());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| invalid())
    }
    /// Preserve browser request IDs and ephemeral headers through the daemon API.
    pub async fn capture(
        &self,
        request: okytes_browser_bridge::protocol::Request,
    ) -> EngineResult<okytes_browser_bridge::protocol::Response> {
        request.validate().map_err(|_| invalid())?;
        self.request(Method::POST, "v1/capture", Some(&request))
            .await
    }
    async fn events(&self) -> EngineResult<reqwest::Response> {
        let response = self
            .http
            .get(self.base.join("v1/events").map_err(|_| invalid())?)
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(3600))
            .send()
            .await
            .map_err(|_| invalid())?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(invalid())
        }
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
            let response = self.events().await?;
            let (tx, rx) = tokio::sync::broadcast::channel(64);
            let client = self.clone();
            tokio::spawn(async move {
                let mut response = response;
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
                        if let Ok(next) = client.events().await {
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
