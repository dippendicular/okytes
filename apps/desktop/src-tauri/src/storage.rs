//! Cloud storage connectors: Google Drive, Dropbox and OneDrive. The user
//! registers their own OAuth app with each provider and enters its client ID;
//! sign-in uses the system browser with PKCE and a fixed loopback redirect.
//! Refresh tokens are private profile files; access tokens stay in memory.
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// Register exactly this redirect URI with each provider.
pub const REDIRECT: &str = "http://127.0.0.1:17901/callback";
const PROVIDERS: [&str; 3] = ["gdrive", "dropbox", "onedrive"];
/// 10 MiB: a multiple of both Google's 256 KiB and OneDrive's 320 KiB units.
const CHUNK: u64 = 10 * 1024 * 1024;

struct Endpoints {
    authorize: &'static str,
    token: &'static str,
    scope: &'static str,
}
fn endpoints(provider: &str) -> Option<Endpoints> {
    Some(match provider {
        "gdrive" => Endpoints {
            authorize: "https://accounts.google.com/o/oauth2/v2/auth",
            token: "https://oauth2.googleapis.com/token",
            scope: "https://www.googleapis.com/auth/drive.file",
        },
        "dropbox" => Endpoints {
            authorize: "https://www.dropbox.com/oauth2/authorize",
            token: "https://api.dropboxapi.com/oauth2/token",
            scope: "files.content.write",
        },
        "onedrive" => Endpoints {
            authorize: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            token: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            scope: "Files.ReadWrite offline_access",
        },
        _ => return None,
    })
}
pub fn label(provider: &str) -> &'static str {
    match provider {
        "gdrive" => "Google Drive",
        "dropbox" => "Dropbox",
        _ => "OneDrive",
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Client {
    client_id: String,
    /// Google desktop clients issue a non-confidential secret the token
    /// endpoint still requires; Dropbox and Microsoft PKCE clients need none.
    #[serde(default)]
    client_secret: Option<String>,
}

#[derive(Serialize)]
pub struct ProviderStatus {
    pub provider: String,
    pub name: String,
    pub configured: bool,
    pub connected: bool,
}

pub struct Storage {
    directory: PathBuf,
    access: Mutex<HashMap<String, (String, Instant)>>,
    signing_in: tokio::sync::Mutex<()>,
}

fn invalid(provider: &str) -> String {
    format!("Unknown storage provider {provider}")
}
fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("okytes-desktop")
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())
}
async fn json(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        Ok(body)
    } else {
        let detail = body
            .pointer("/error/message")
            .or_else(|| body.get("error_description"))
            .or_else(|| body.get("error_summary"))
            .and_then(Value::as_str)
            .unwrap_or("request refused");
        Err(format!("{status}: {detail}"))
    }
}

impl Storage {
    pub fn new(directory: &Path) -> Self {
        Self {
            directory: directory.into(),
            access: Mutex::new(HashMap::new()),
            signing_in: tokio::sync::Mutex::new(()),
        }
    }
    fn clients(&self) -> HashMap<String, Client> {
        std::fs::read(self.directory.join("storage.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
    fn token_path(&self, provider: &str) -> PathBuf {
        self.directory.join(format!("storage-{provider}.token"))
    }
    fn refresh_token(&self, provider: &str) -> Option<String> {
        let bytes = okytes_browser_bridge::read_secret(&self.token_path(provider)).ok()?;
        String::from_utf8(bytes).ok()
    }

    pub fn status(&self) -> Vec<ProviderStatus> {
        let clients = self.clients();
        PROVIDERS
            .iter()
            .map(|p| ProviderStatus {
                provider: (*p).into(),
                name: label(p).into(),
                configured: clients.contains_key(*p),
                connected: self.refresh_token(p).is_some(),
            })
            .collect()
    }
    pub fn configure(
        &self,
        provider: &str,
        client_id: String,
        secret: Option<String>,
    ) -> Result<(), String> {
        endpoints(provider).ok_or_else(|| invalid(provider))?;
        let client_id = client_id.trim().to_owned();
        if client_id.is_empty()
            || client_id.len() > 256
            || client_id.chars().any(char::is_whitespace)
        {
            return Err("Enter the client ID from the provider's developer console".into());
        }
        let mut clients = self.clients();
        clients.insert(
            provider.into(),
            Client {
                client_id,
                client_secret: secret
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty()),
            },
        );
        std::fs::write(
            self.directory.join("storage.json"),
            serde_json::to_vec_pretty(&clients).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
    pub fn disconnect(&self, provider: &str) -> Result<(), String> {
        if let Ok(mut access) = self.access.lock() {
            access.remove(provider);
        }
        match std::fs::remove_file(self.token_path(provider)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Browser sign-in. Listens once on the fixed loopback redirect.
    pub async fn connect(&self, provider: &str) -> Result<(), String> {
        let endpoints = endpoints(provider).ok_or_else(|| invalid(provider))?;
        let client = self
            .clients()
            .remove(provider)
            .ok_or("Enter this provider's client ID first")?;
        let _guard = self
            .signing_in
            .try_lock()
            .map_err(|_| "Another sign-in is already waiting in the browser")?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:17901")
            .await
            .map_err(|_| "Port 17901 is in use; close the other sign-in and retry")?;
        let random = |n: usize| -> String {
            let bytes: Vec<u8> = (0..n).map(|_| rand::random::<u8>()).collect();
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        };
        let (state, verifier) = (random(16), random(48));
        let challenge =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha256(verifier.as_bytes()));
        let mut url = url::Url::parse(endpoints.authorize).map_err(|e| e.to_string())?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("client_id", &client.client_id)
                .append_pair("redirect_uri", REDIRECT)
                .append_pair("response_type", "code")
                .append_pair("state", &state)
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256");
            match provider {
                // Dropbox issues refresh tokens only for offline access.
                "dropbox" => query.append_pair("token_access_type", "offline"),
                // Google returns a refresh token only with offline consent.
                "gdrive" => query
                    .append_pair("scope", endpoints.scope)
                    .append_pair("access_type", "offline")
                    .append_pair("prompt", "consent"),
                _ => query.append_pair("scope", endpoints.scope),
            };
        }
        open_browser(url.as_str())?;
        let code = tokio::time::timeout(Duration::from_secs(300), async {
            loop {
                let (mut stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
                let mut buffer = vec![0u8; 8192];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let Some(target) = request.lines().next().and_then(|l| l.split(' ').nth(1)) else {
                    continue;
                };
                let Ok(callback) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
                    continue;
                };
                if callback.path() != "/callback" {
                    let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n").await;
                    continue;
                }
                let params: HashMap<_, _> = callback.query_pairs().into_owned().collect();
                let ok = params.get("state") == Some(&state) && params.contains_key("code");
                let page = if ok {
                    "Okytes is connected. You can close this tab."
                } else {
                    "Okytes could not complete the sign-in. Return to Okytes and try again."
                };
                let _ = stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{page}",
                            page.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                if params.get("state") != Some(&state) {
                    continue; // Unrelated or forged request; keep waiting.
                }
                return params
                    .get("code")
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "{} sign-in was cancelled ({})",
                            label(provider),
                            params.get("error").map(String::as_str).unwrap_or("no code")
                        )
                    });
            }
        })
        .await
        .map_err(|_| "Sign-in timed out after five minutes".to_owned())??;
        let mut form = vec![
            ("grant_type", "authorization_code".to_owned()),
            ("code", code),
            ("redirect_uri", REDIRECT.into()),
            ("client_id", client.client_id.clone()),
            ("code_verifier", verifier),
        ];
        if let Some(secret) = client.client_secret {
            form.push(("client_secret", secret));
        }
        let token = self.token_request(endpoints.token, &form).await?;
        let refresh = token
            .get("refresh_token")
            .and_then(Value::as_str)
            .ok_or("The provider did not grant offline access (no refresh token)")?;
        let _ = std::fs::remove_file(self.token_path(provider));
        okytes_browser_bridge::write_secret(&self.token_path(provider), refresh.as_bytes())
            .map_err(|_| "The sign-in could not be saved".to_owned())?;
        self.remember(provider, &token);
        Ok(())
    }

    async fn token_request(
        &self,
        endpoint: &str,
        form: &[(&str, String)],
    ) -> Result<Value, String> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form.iter().map(|(k, v)| (*k, v.as_str())))
            .finish();
        json(
            http()?
                .post(endpoint)
                .header("content-type", "application/x-www-form-urlencoded")
                .header("accept", "application/json")
                .timeout(Duration::from_secs(30))
                .body(body)
                .send()
                .await
                .map_err(|e| e.to_string())?,
        )
        .await
    }
    fn remember(&self, provider: &str, token: &Value) {
        if let (Some(access), Ok(mut cache)) = (
            token.get("access_token").and_then(Value::as_str),
            self.access.lock(),
        ) {
            let lifetime = token
                .get("expires_in")
                .and_then(Value::as_u64)
                .unwrap_or(3600);
            cache.insert(
                provider.into(),
                (
                    access.into(),
                    Instant::now() + Duration::from_secs(lifetime.saturating_sub(120)),
                ),
            );
        }
    }
    async fn access_token(&self, provider: &str) -> Result<String, String> {
        if let Some((token, until)) = self
            .access
            .lock()
            .ok()
            .and_then(|c| c.get(provider).cloned())
        {
            if Instant::now() < until {
                return Ok(token);
            }
        }
        let endpoints = endpoints(provider).ok_or_else(|| invalid(provider))?;
        let client = self
            .clients()
            .remove(provider)
            .ok_or("Storage provider is not configured")?;
        let refresh = self
            .refresh_token(provider)
            .ok_or_else(|| format!("Connect {} in Settings first", label(provider)))?;
        let mut form = vec![
            ("grant_type", "refresh_token".to_owned()),
            ("refresh_token", refresh),
            ("client_id", client.client_id),
        ];
        if let Some(secret) = client.client_secret {
            form.push(("client_secret", secret));
        }
        let token = self
            .token_request(endpoints.token, &form)
            .await
            .map_err(|e| {
                format!(
                    "{} sign-in expired or was revoked; reconnect it in Settings ({e})",
                    label(provider)
                )
            })?;
        // Providers may rotate refresh tokens; keep the newest one.
        if let Some(rotated) = token.get("refresh_token").and_then(Value::as_str) {
            let _ = std::fs::remove_file(self.token_path(provider));
            let _ =
                okytes_browser_bridge::write_secret(&self.token_path(provider), rotated.as_bytes());
        }
        self.remember(provider, &token);
        token
            .get("access_token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "The provider returned no access token".into())
    }

    /// Upload one completed file into an `Okytes` folder with resumable,
    /// chunked requests so large files never load fully into memory.
    pub async fn upload(
        &self,
        provider: &str,
        path: &Path,
        progress: Progress<'_>,
    ) -> Result<(), String> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("The downloaded file has no usable name")?
            .to_owned();
        let size = tokio::fs::metadata(path)
            .await
            .map_err(|e| e.to_string())?
            .len();
        let token = self.access_token(provider).await?;
        let client = http()?;
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|e| e.to_string())?;
        match provider {
            "gdrive" => {
                let folder = self.drive_folder(&client, &token).await?;
                let session = client
                    .post("https://www.googleapis.com/upload/drive/v3/files?uploadType=resumable")
                    .bearer_auth(&token)
                    .header("x-upload-content-length", size)
                    .json(&serde_json::json!({ "name": name, "parents": [folder] }))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                let location = session
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
                    .ok_or(
                        json(session)
                            .await
                            .err()
                            .unwrap_or_else(|| "Drive gave no upload session".into()),
                    )?;
                put_chunks(&client, provider, &location, &mut file, size, progress).await
            }
            "onedrive" => {
                let encoded: String =
                    url::form_urlencoded::byte_serialize(name.as_bytes()).collect();
                let session = json(
                    client
                        .post(format!(
                            "https://graph.microsoft.com/v1.0/me/drive/root:/Okytes/{}:/createUploadSession",
                            encoded.replace('+', "%20")
                        ))
                        .bearer_auth(&token)
                        .json(&serde_json::json!({ "item": { "@microsoft.graph.conflictBehavior": "rename" } }))
                        .send()
                        .await
                        .map_err(|e| e.to_string())?,
                )
                .await?;
                let location = session
                    .get("uploadUrl")
                    .and_then(Value::as_str)
                    .ok_or("OneDrive gave no upload session")?;
                put_chunks(&client, provider, location, &mut file, size, progress).await
            }
            _ => {
                let start = json(
                    client
                        .post("https://content.dropboxapi.com/2/files/upload_session/start")
                        .bearer_auth(&token)
                        .header("dropbox-api-arg", r#"{"close":false}"#)
                        .header("content-type", "application/octet-stream")
                        .body(Vec::new())
                        .send()
                        .await
                        .map_err(|e| e.to_string())?,
                )
                .await?;
                let session = start
                    .get("session_id")
                    .and_then(Value::as_str)
                    .ok_or("Dropbox gave no upload session")?
                    .to_owned();
                let mut offset = 0u64;
                while offset < size {
                    let chunk = read_chunk(&mut file, offset, CHUNK.min(size - offset)).await?;
                    let arg = serde_json::json!({ "cursor": { "session_id": session, "offset": offset }, "close": false });
                    let mut attempt = 0;
                    loop {
                        let sent = client
                            .post("https://content.dropboxapi.com/2/files/upload_session/append_v2")
                            .bearer_auth(&token)
                            .header("dropbox-api-arg", arg.to_string())
                            .header("content-type", "application/octet-stream")
                            .body(chunk.clone())
                            .send()
                            .await
                            .map_err(|e| e.to_string());
                        let result = match sent {
                            Ok(response) => json(response).await,
                            Err(error) => Err(error),
                        };
                        match result {
                            Ok(_) => break,
                            // Appends are atomic: after a retry, incorrect_offset
                            // means the earlier attempt of this chunk was stored.
                            Err(error) if attempt > 0 && error.contains("incorrect_offset") => {
                                break
                            }
                            Err(error) => {
                                attempt += 1;
                                if attempt >= ATTEMPTS {
                                    return Err(format!(
                                        "upload failed after {ATTEMPTS} attempts: {error}"
                                    ));
                                }
                                tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                            }
                        }
                    }
                    offset += chunk.len() as u64;
                    progress(offset);
                }
                let finish = serde_json::json!({
                    "cursor": { "session_id": session, "offset": size },
                    "commit": { "path": format!("/Okytes/{name}"), "mode": "add", "autorename": true },
                });
                json(
                    client
                        .post("https://content.dropboxapi.com/2/files/upload_session/finish")
                        .bearer_auth(&token)
                        .header("dropbox-api-arg", ascii_json(&finish))
                        .header("content-type", "application/octet-stream")
                        .body(Vec::new())
                        .send()
                        .await
                        .map_err(|e| e.to_string())?,
                )
                .await
                .map(|_| ())
            }
        }
    }

    /// Find or create the `Okytes` folder. The drive.file scope only sees
    /// files and folders this app created, which is exactly what we want.
    async fn drive_folder(&self, client: &reqwest::Client, token: &str) -> Result<String, String> {
        let query = "name = 'Okytes' and mimeType = 'application/vnd.google-apps.folder' and trashed = false";
        let found = json(
            client
                .get("https://www.googleapis.com/drive/v3/files")
                .bearer_auth(token)
                .query(&[("q", query), ("fields", "files(id)"), ("spaces", "drive")])
                .send()
                .await
                .map_err(|e| e.to_string())?,
        )
        .await?;
        if let Some(id) = found.pointer("/files/0/id").and_then(Value::as_str) {
            return Ok(id.into());
        }
        let created = json(
            client
                .post("https://www.googleapis.com/drive/v3/files")
                .bearer_auth(token)
                .json(&serde_json::json!({ "name": "Okytes", "mimeType": "application/vnd.google-apps.folder" }))
                .send()
                .await
                .map_err(|e| e.to_string())?,
        )
        .await?;
        created
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "Drive did not create the Okytes folder".into())
    }
}

// Dropbox-API-Arg is an HTTP header, so non-ASCII must be JSON-escaped.
fn ascii_json(value: &Value) -> String {
    value
        .to_string()
        .chars()
        .map(|c| {
            if c.is_ascii() {
                c.to_string()
            } else {
                c.encode_utf16(&mut [0; 2])
                    .iter()
                    .map(|u| format!("\\u{u:04x}"))
                    .collect()
            }
        })
        .collect()
}

async fn read_chunk(file: &mut tokio::fs::File, offset: u64, len: u64) -> Result<Vec<u8>, String> {
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|e| e.to_string())?;
    let mut chunk = vec![0u8; len as usize];
    file.read_exact(&mut chunk)
        .await
        .map_err(|e| e.to_string())?;
    Ok(chunk)
}

/// Attempts per chunk before an upload is reported as failed.
const ATTEMPTS: u32 = 4;
/// Progress callback: bytes the provider has confirmed so far.
pub type Progress<'a> = &'a (dyn Fn(u64) + Send + Sync);

/// Google Drive and OneDrive resumable sessions: PUT ranges until the server
/// reports completion (200/201); 308/202 mean "send the next range". After a
/// failed chunk the session is asked how much it actually received, so the
/// upload resumes from the provider's offset rather than guessing.
async fn put_chunks(
    client: &reqwest::Client,
    provider: &str,
    location: &str,
    file: &mut tokio::fs::File,
    size: u64,
    progress: Progress<'_>,
) -> Result<(), String> {
    let mut offset = 0u64;
    let mut failures = 0u32;
    loop {
        let len = CHUNK.min(size - offset);
        let chunk = read_chunk(file, offset, len).await?;
        let range = if size == 0 {
            "bytes */0".to_owned()
        } else {
            format!("bytes {offset}-{}/{size}", offset + len - 1)
        };
        // Session URLs are pre-authorized; they must not receive the bearer token.
        let sent = client
            .put(location)
            .header("content-range", range)
            .header("content-length", len)
            .timeout(Duration::from_secs(600))
            .body(chunk)
            .send()
            .await;
        let outcome = match sent {
            Ok(response) => match response.status().as_u16() {
                200 | 201 => {
                    progress(size);
                    return Ok(());
                }
                202 | 308 => Ok(()),
                code if code == 429 || code >= 500 => Err(format!("provider returned {code}")),
                _ => return json(response).await.map(|_| ()),
            },
            Err(error) => Err(error.to_string()),
        };
        match outcome {
            Ok(()) => {
                offset += len;
                failures = 0;
                progress(offset);
            }
            Err(error) => {
                failures += 1;
                if failures >= ATTEMPTS {
                    return Err(format!("upload failed after {ATTEMPTS} attempts: {error}"));
                }
                tokio::time::sleep(Duration::from_secs(2u64.pow(failures))).await;
                match received(client, provider, location, size).await? {
                    None => {
                        progress(size);
                        return Ok(());
                    }
                    Some(confirmed) => offset = confirmed,
                }
            }
        }
        if offset >= size {
            return Err("Upload ended without the provider confirming the file".into());
        }
    }
}

/// Ask a resumable session how many bytes it holds. `None` means complete.
async fn received(
    client: &reqwest::Client,
    provider: &str,
    location: &str,
    size: u64,
) -> Result<Option<u64>, String> {
    if provider == "onedrive" {
        let status = json(
            client
                .get(location)
                .send()
                .await
                .map_err(|e| e.to_string())?,
        )
        .await?;
        let next = status
            .pointer("/nextExpectedRanges/0")
            .and_then(Value::as_str)
            .and_then(|r| r.split('-').next())
            .and_then(|n| n.parse().ok());
        return Ok(next.or(Some(0)));
    }
    let response = client
        .put(location)
        .header("content-range", format!("bytes */{size}"))
        .header("content-length", 0)
        .body(Vec::new())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    match response.status().as_u16() {
        200 | 201 => Ok(None),
        308 => Ok(Some(
            response
                .headers()
                .get("range")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit('-').next())
                .and_then(|n| n.parse::<u64>().ok())
                .map_or(0, |last| last + 1),
        )),
        _ => json(response).await.map(|_| Some(0)),
    }
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).to_vec()
}

fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    result
        .map(|_| ())
        .map_err(|_| "Could not open the browser for sign-in".into())
}
