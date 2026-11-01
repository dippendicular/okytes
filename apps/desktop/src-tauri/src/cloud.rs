//! Optional sync with a self-hosted `okytes-cloud` server. The device token is
//! a private profile file, like the browser-bridge token; the server URL and
//! device ID sit beside it. Nothing runs until the user pairs this desktop.
use okytes_core::desktop::DesktopService;
use okytes_desktop_service::Session;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const TOKEN: &str = "cloud-device.token";
const CONFIG: &str = "cloud.json";

#[derive(Serialize, Deserialize, Clone)]
pub struct CloudStatus {
    pub url: String,
    pub device_id: String,
    pub name: String,
    /// Last sync failure, if any; cleared after a successful round.
    #[serde(default)]
    pub error: Option<String>,
}

pub struct Cloud {
    directory: PathBuf,
    error: std::sync::Mutex<Option<String>>,
    /// Latest history from this account's other devices (memory only).
    remote: std::sync::Mutex<Vec<Value>>,
}

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("okytes-desktop")
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())
}

// Signed links carry credentials in their query; the server never needs them.
fn redact(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or_default().to_owned()
}

async fn send(request: reqwest::RequestBuilder) -> Result<Value, String> {
    let response = request.send().await.map_err(|_| {
        "Okytes Cloud is unreachable. Check the server address and that it is running.".to_owned()
    })?;
    let status = response.status();
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status.is_success() {
        Ok(body)
    } else {
        Err(body
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Okytes Cloud returned {status}")))
    }
}

impl Cloud {
    pub fn new(directory: &Path) -> Arc<Self> {
        Arc::new(Self {
            directory: directory.into(),
            error: std::sync::Mutex::new(None),
            remote: std::sync::Mutex::new(Vec::new()),
        })
    }
    fn config(&self) -> Option<(CloudStatus, String)> {
        let config: CloudStatus =
            serde_json::from_slice(&std::fs::read(self.directory.join(CONFIG)).ok()?).ok()?;
        let token = okytes_browser_bridge::read_secret(&self.directory.join(TOKEN)).ok()?;
        Some((config, String::from_utf8(token).ok()?.trim().to_owned()))
    }
    pub fn status(&self) -> Option<CloudStatus> {
        self.config().map(|(mut status, _)| {
            status.error = self.error.lock().ok().and_then(|e| e.clone());
            status
        })
    }
    pub async fn pair(
        &self,
        url: String,
        code: String,
        name: String,
    ) -> Result<CloudStatus, String> {
        let base = url.trim().trim_end_matches('/').to_owned();
        let parsed = url::Url::parse(&base)
            .map_err(|_| "Enter the server address, e.g. http://127.0.0.1:17900".to_owned())?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("The server address must start with http:// or https://".into());
        }
        if parsed.scheme() == "http"
            && !matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        {
            return Err("Use https:// for a server that is not on this computer.".into());
        }
        let result = send(
            http()?
                .post(format!("{base}/v1/devices/claim"))
                .json(&json!({
                    "code": code.trim(),
                    "name": name.trim(),
                    "kind": "desktop",
                })),
        )
        .await?;
        let token = result["token"]
            .as_str()
            .ok_or("Server sent no device token")?;
        let device_id = result["device_id"]
            .as_str()
            .ok_or("Server sent no device ID")?;
        // Replace any previous pairing; the private writer refuses to overwrite.
        let _ = std::fs::remove_file(self.directory.join(TOKEN));
        okytes_browser_bridge::write_secret(&self.directory.join(TOKEN), token.as_bytes())
            .map_err(|_| "The device token could not be saved".to_owned())?;
        let status = CloudStatus {
            url: base,
            device_id: device_id.into(),
            name: name.trim().into(),
            error: None,
        };
        std::fs::write(
            self.directory.join(CONFIG),
            serde_json::to_vec_pretty(&status).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(status)
    }
    /// History synced from this account's other devices, newest first.
    pub fn remote_history(&self) -> Vec<Value> {
        self.remote.lock().map(|r| r.clone()).unwrap_or_default()
    }
    pub fn unpair(&self) -> Result<(), String> {
        if let Ok(mut remote) = self.remote.lock() {
            remote.clear();
        }
        for file in [TOKEN, CONFIG] {
            match std::fs::remove_file(self.directory.join(file)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    }

    /// Pull queued links every ten seconds and push history once a minute.
    pub async fn run(self: Arc<Self>, session: Arc<Session>) {
        let mut pushed: HashMap<String, String> = HashMap::new();
        let mut round = 0u32;
        loop {
            if let Some((config, token)) = self.config() {
                let result = self
                    .sync(
                        &session,
                        &config.url,
                        &token,
                        round.is_multiple_of(6),
                        &mut pushed,
                    )
                    .await;
                if let Err(error) = &result {
                    tracing::warn!(%error, "cloud sync failed");
                }
                if let Ok(mut slot) = self.error.lock() {
                    *slot = result.err();
                }
            } else {
                pushed.clear();
            }
            round = round.wrapping_add(1);
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }

    async fn sync(
        &self,
        session: &Session,
        base: &str,
        token: &str,
        history: bool,
        pushed: &mut HashMap<String, String>,
    ) -> Result<(), String> {
        let client = http()?;
        let claimed = send(
            client
                .post(format!("{base}/v1/queue/claim"))
                .bearer_auth(token),
        )
        .await?;
        for item in claimed["items"].as_array().cloned().unwrap_or_default() {
            let (Some(id), Some(url)) = (item["id"].as_str(), item["url"].as_str()) else {
                continue;
            };
            // The queue UUID is the durable capture ID: re-claiming after a
            // crash returns the existing job instead of adding a duplicate.
            let outcome = session
                .capture(
                    id.into(),
                    okytes_browser_bridge::protocol::Capture {
                        url: url.into(),
                        filename: item["filename"].as_str().map(str::to_owned),
                        sha256: None,
                        headers: vec![],
                    },
                )
                .await;
            let report = match &outcome {
                Ok(job) => json!({ "ok": true, "job_id": job }),
                Err(error) => json!({ "ok": false, "error": error.to_string() }),
            };
            send(
                client
                    .post(format!("{base}/v1/queue/{id}/result"))
                    .bearer_auth(token)
                    .json(&report),
            )
            .await?;
        }
        if history {
            let page = session.list(0).await.map_err(|e| e.to_string())?;
            let changed: Vec<_> = page
                .jobs
                .iter()
                .filter(|job| pushed.get(&job.id) != Some(&job.updated_at))
                .take(500)
                .collect();
            if !changed.is_empty() {
                let entries: Vec<_> = changed
                    .iter()
                    .map(|job| {
                        json!({
                            "job_id": job.id,
                            "name": job.name,
                            "source": redact(job.url.trim_start_matches("hls+").trim_start_matches("dash+")),
                            "state": job.state,
                            "bytes": job.bytes,
                            "total": job.total,
                            "created_at": job.created_at,
                            "updated_at": job.updated_at,
                        })
                    })
                    .collect();
                send(
                    client
                        .put(format!("{base}/v1/history"))
                        .bearer_auth(token)
                        .json(&json!({ "entries": entries })),
                )
                .await?;
                for job in changed {
                    pushed.insert(job.id.clone(), job.updated_at.clone());
                }
            }
            // Pull the other direction: what this account downloaded elsewhere.
            let own = self.config().map(|(c, _)| c.device_id).unwrap_or_default();
            let history = send(
                client
                    .get(format!("{base}/v1/history?limit=500"))
                    .bearer_auth(token),
            )
            .await?;
            let others: Vec<Value> = history["entries"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|entry| entry["device_id"].as_str() != Some(own.as_str()))
                .collect();
            if let Ok(mut remote) = self.remote.lock() {
                *remote = others;
            }
        }
        Ok(())
    }
}
