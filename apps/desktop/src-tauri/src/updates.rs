//! Manual updates: authenticated metadata, bounded downloads, explicit install consent.
use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures::StreamExt;
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::Serialize;
use std::{path::PathBuf, time::Duration};
use tauri::{Manager, State};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Mutex;

const FEED: &str = "https://github.com/dippendicular/okytes/releases/latest/download/latest.json";
const MAX_INSTALLER: usize = 512 * 1024 * 1024;
pub struct Updates(pub Mutex<Option<Pending>>);
pub struct Pending {
    update: Update,
    verified_bytes: Option<Vec<u8>>,
}
#[derive(Serialize)]
pub struct Offer {
    version: String,
    notes: String,
}

fn key() -> Result<&'static str, String> {
    option_env!("OKYTES_UPDATE_PUBLIC_KEY")
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| {
            "Updates are unavailable in this build: no release verification key was embedded."
                .into()
        })
}
fn verify(key: &str, signature: &str, bytes: &[u8]) -> Result<(), String> {
    let decode = |value: &str| -> Result<String, String> {
        String::from_utf8(STANDARD.decode(value.trim()).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    };
    PublicKey::decode(&decode(key)?)
        .map_err(|e| e.to_string())?
        .verify(
            bytes,
            &Signature::decode(&decode(signature)?).map_err(|e| e.to_string())?,
            false,
        )
        .map_err(|_| "Release signature verification failed; nothing will be installed.".into())
}
fn signed_metadata(bytes: &[u8], key: &str) -> Result<serde_json::Value, String> {
    let envelope: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let payload = envelope["payload"]
        .as_str()
        .ok_or("Missing signed release metadata")?;
    let signature = envelope["signature"]
        .as_str()
        .ok_or("Missing metadata signature")?;
    let payload = STANDARD.decode(payload).map_err(|e| e.to_string())?;
    verify(key, signature, &payload)?;
    serde_json::from_slice(&payload).map_err(|e| e.to_string())
}
fn newer(version: &str, current: &str, floor: Option<&str>) -> Result<(), String> {
    let version = Version::parse(version).map_err(|e| e.to_string())?;
    let current = Version::parse(current).map_err(|e| e.to_string())?;
    if !version.pre.is_empty() || version <= current {
        return Err("Downgrades, replayed versions and prerelease updates are refused.".into());
    }
    if let Some(floor) = floor {
        let floor = Version::parse(floor).map_err(|e| e.to_string())?;
        if version < floor {
            return Err("Update is older than a previously verified release.".into());
        }
    }
    Ok(())
}
async fn fetch(url: reqwest::Url, maximum: usize) -> Result<Vec<u8>, String> {
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Updates require an HTTPS URL without credentials.".into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .connect_timeout(Duration::from_secs(20))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    read_bounded(response, maximum).await
}
async fn read_bounded(response: reqwest::Response, maximum: usize) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|size| size > maximum as u64)
    {
        return Err("Update exceeds the download size limit.".into());
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        if bytes.len().saturating_add(chunk.len()) > maximum {
            return Err("Update exceeds the download size limit.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn floor_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let directory = match std::env::var_os("OKYTES_DATA_DIR") {
        Some(directory) => PathBuf::from(directory),
        None => app.path().app_data_dir().map_err(|e| e.to_string())?,
    };
    if !directory.is_absolute() {
        return Err("OKYTES_DATA_DIR must be absolute".into());
    }
    Ok(directory.join("verified-update-version"))
}
fn read_floor(app: &tauri::AppHandle) -> Result<Option<String>, String> {
    load_floor(&floor_path(app)?)
}
fn load_floor(path: &std::path::Path) -> Result<Option<String>, String> {
    use std::io::Read;
    match std::fs::File::open(path) {
        Ok(file) => {
            let mut value = String::new();
            file.take(129)
                .read_to_string(&mut value)
                .map_err(|e| e.to_string())?;
            if value.len() > 128 {
                return Err("Invalid persisted update version".into());
            }
            Version::parse(&value).map_err(|e| e.to_string())?;
            Ok(Some(value))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
fn store_floor(path: &std::path::Path, version: &str) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temporary = path.with_extension("new");
    let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
    file.write_all(version.as_bytes())
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&temporary, path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[tauri::command]
pub async fn check_update(
    app: tauri::AppHandle,
    state: State<'_, Updates>,
) -> Result<Option<Offer>, String> {
    let mut pending = state
        .0
        .try_lock()
        .map_err(|_| "An update operation is already running")?;
    *pending = None;
    let key = key()?;
    if matches!(
        tauri::utils::platform::bundle_type(),
        Some(tauri::utils::config::BundleType::Deb | tauri::utils::config::BundleType::Rpm)
    ) {
        return Err("Update this installation with your Linux package manager.".into());
    }
    let endpoint = reqwest::Url::parse(FEED).map_err(|e| e.to_string())?;
    let bytes = fetch(endpoint.clone(), 1024 * 1024).await?;
    let metadata = signed_metadata(&bytes, key)?;
    let updater = app
        .updater_builder()
        .pubkey(key)
        .endpoints(vec![endpoint])
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    let Some(update) = updater
        .restore_update(metadata)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    newer(
        &update.version,
        &app.package_info().version.to_string(),
        read_floor(&app)?.as_deref(),
    )?;
    let offer = Offer {
        version: update.version.clone(),
        notes: update.body.clone().unwrap_or_default(),
    };
    *pending = Some(Pending {
        update,
        verified_bytes: None,
    });
    Ok(Some(offer))
}
#[tauri::command]
pub async fn download_update(
    app: tauri::AppHandle,
    state: State<'_, Updates>,
    confirmed: bool,
) -> Result<(), String> {
    if !confirmed {
        return Err("Download confirmation is required.".into());
    }
    let mut pending = state
        .0
        .try_lock()
        .map_err(|_| "An update operation is already running")?;
    let pending = pending.as_mut().ok_or("Check for an update first")?;
    pending.verified_bytes = None;
    let bytes = fetch(pending.update.download_url.clone(), MAX_INSTALLER).await?;
    verify(key()?, &pending.update.signature, &bytes)?;
    newer(
        &pending.update.version,
        &app.package_info().version.to_string(),
        read_floor(&app)?.as_deref(),
    )?;
    store_floor(&floor_path(&app)?, &pending.update.version)?;
    pending.verified_bytes = Some(bytes);
    Ok(())
}

#[tauri::command]
pub async fn install_update(
    app: tauri::AppHandle,
    state: State<'_, Updates>,
    confirmed: bool,
) -> Result<(), String> {
    if !confirmed {
        return Err("Installation confirmation is required.".into());
    }
    let mut guard = state
        .0
        .try_lock()
        .map_err(|_| "An update operation is already running")?;
    let pending = guard.as_mut().ok_or("No update was checked")?;
    let bytes = pending
        .verified_bytes
        .as_ref()
        .ok_or("Download and verify the update first")?;
    newer(
        &pending.update.version,
        &app.package_info().version.to_string(),
        read_floor(&app)?.as_deref(),
    )?;
    pending.update.install(bytes).map_err(|e| e.to_string())?;
    *guard = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: &str = include_str!("../tests/fixtures/update.pub");
    const PAYLOAD: &[u8] = include_bytes!("../tests/fixtures/payload.txt");
    const SIGNATURE: &str = include_str!("../tests/fixtures/payload.sig");
    const WRONG: &str = include_str!("../tests/fixtures/wrong.sig");
    #[test]
    fn base_configuration_initializes_updater_without_a_release_key() {
        let configuration: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let updater: tauri_plugin_updater::Config =
            serde_json::from_value(configuration["plugins"]["updater"].clone()).unwrap();
        assert!(updater.pubkey.is_empty());
        assert!(updater.require_signed_version);
        assert!(!updater.allow_downgrades);
    }

    async fn server(bytes: Vec<u8>) -> reqwest::Response {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(&bytes).await.unwrap();
        });
        reqwest::get(format!("http://{address}/update"))
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn local_update_server_valid_tampered_wrong_key_and_size_limit() {
        let bytes = read_bounded(server(PAYLOAD.to_vec()).await, 1024)
            .await
            .unwrap();
        verify(KEY, SIGNATURE, &bytes).unwrap();
        assert!(verify(KEY, WRONG, &bytes).is_err());
        let mut tampered = PAYLOAD.to_vec();
        tampered[0] ^= 1;
        let bytes = read_bounded(server(tampered).await, 1024).await.unwrap();
        assert!(verify(KEY, SIGNATURE, &bytes).is_err());
        assert!(read_bounded(server(vec![0; 1025]).await, 1024)
            .await
            .is_err());
    }
    #[test]
    fn downgrades_replay_and_malformed_versions_are_refused() {
        assert!(newer("0.2.0", "0.1.0", None).is_ok());
        assert!(newer("0.2.0", "0.1.0", Some("0.3.0")).is_err());
        assert!(newer("0.1.0", "0.1.0", None).is_err());
        assert!(newer("0.0.9", "0.1.0", None).is_err());
        assert!(newer("0.2.0-beta.1", "0.1.0", None).is_err());
        assert!(newer("invalid", "0.1.0", None).is_err());
        assert!(newer("0.2.0", "0.1.0", Some("corrupted")).is_err());
    }
    #[test]
    fn unsigned_metadata_is_never_trusted() {
        assert!(signed_metadata(br#"{"version":"100.0.0"}"#, KEY).is_err());
        let envelope = serde_json::json!({"payload":STANDARD.encode(PAYLOAD),"signature":WRONG});
        assert!(signed_metadata(&serde_json::to_vec(&envelope).unwrap(), KEY).is_err());
    }
    #[test]
    fn replay_floor_survives_reopen_and_rejects_corrupt_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("verified-update-version");
        assert_eq!(load_floor(&path).unwrap(), None);
        store_floor(&path, "0.2.0").unwrap();
        assert_eq!(load_floor(&path).unwrap().as_deref(), Some("0.2.0"));
        store_floor(&path, "0.3.0").unwrap();
        assert!(newer("0.2.0", "0.1.0", load_floor(&path).unwrap().as_deref()).is_err());
        std::fs::write(&path, "invalid").unwrap();
        assert!(load_floor(&path).is_err());
        std::fs::write(&path, vec![b'a'; 1024]).unwrap();
        assert!(load_floor(&path).is_err());
    }
    #[test]
    fn release_notes_version_and_platforms_are_authenticated_together() {
        let payload = include_bytes!("../tests/fixtures/metadata.json");
        let signature = include_str!("../tests/fixtures/metadata.sig");
        let mut envelope =
            serde_json::json!({"payload":STANDARD.encode(payload),"signature":signature});
        let metadata = signed_metadata(&serde_json::to_vec(&envelope).unwrap(), KEY).unwrap();
        assert_eq!(metadata["version"], "0.2.0");
        envelope["payload"] = STANDARD
            .encode(br#"{"version":"100.0.0","notes":"malicious","platforms":{}}"#)
            .into();
        assert!(signed_metadata(&serde_json::to_vec(&envelope).unwrap(), KEY).is_err());
    }
    #[test]
    fn release_script_feed_round_trip_preserves_installer_types() {
        // Generated by packaging/sign-release.mjs with synthetic artifacts and
        // a test-only key; no production release or installer is represented.
        let metadata =
            signed_metadata(include_bytes!("../tests/fixtures/release-feed.json"), KEY).unwrap();
        let _: tauri_plugin_updater::RemoteRelease =
            serde_json::from_value(metadata.clone()).unwrap();
        assert_eq!(metadata["version"], "0.1.0");
        assert_eq!(metadata["platforms"].as_object().unwrap().len(), 6);
        assert!(metadata["platforms"]["windows-x86_64-nsis"]["url"]
            .as_str()
            .unwrap()
            .ends_with("-setup.exe"));
        assert!(metadata["platforms"]["linux-aarch64-appimage"]["url"]
            .as_str()
            .unwrap()
            .ends_with(".AppImage"));
        assert!(metadata["platforms"].get("windows-x86_64").is_none());
    }
}
