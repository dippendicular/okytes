#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use okytes_core::desktop::{Action, AddInput, Job, Settings, Snapshot};
use okytes_desktop_service::{AppService, Session};
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;
mod automation;
mod cloud;
mod merge;
mod storage;
mod updates;

const CHROMIUM_EXTENSION_ID: &str = "lodlcbjagldkbkplgkhmhklfgpbgmpfb";

fn register_native_host() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or("desktop executable has no parent directory")?;
    #[cfg(target_os = "windows")]
    let host = directory.join("okytes-native-host.exe");
    #[cfg(not(target_os = "windows"))]
    let host = directory.join("okytes-native-host");

    if !host.is_file() {
        return Err(format!("native host companion is missing at {}", host.display()).into());
    }
    let output = std::process::Command::new(host)
        .args(["install", "--chromium-id", CHROMIUM_EXTENSION_ID])
        .output()?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        return Err(format!("native host registration failed: {}", message.trim()).into());
    }
    Ok(())
}

#[tauri::command]
fn read_clipboard() -> Result<String, String> {
    #[cfg(target_os = "windows")]
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-Clipboard -Raw",
        ])
        .output();
    #[cfg(target_os = "macos")]
    let output = std::process::Command::new("pbpaste").output();
    #[cfg(all(unix, not(target_os = "macos")))]
    let output = std::process::Command::new("xclip")
        .args(["-selection", "clipboard", "-o"])
        .output();
    let output = output.map_err(|_| "Clipboard is unavailable".to_owned())?;
    if !output.status.success() {
        return Err("Clipboard is unavailable".to_owned());
    }
    String::from_utf8(output.stdout).map_err(|_| "Clipboard does not contain text".to_owned())
}

fn daemon_client() -> Result<okytes_daemon_api::Client, String> {
    okytes_daemon_api::Client::from_environment()
        .map_err(|_| "Invalid daemon configuration".to_owned())?
        .ok_or_else(|| "LAN Share requires the Okytes daemon".to_owned())
}

#[tauri::command]
async fn peer_identity() -> Result<okytes_daemon_api::contract::PeerIdentity, String> {
    daemon_client()?
        .request(reqwest::Method::GET, "v1/peers/identity", None::<&()>)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn peer_discover() -> Result<Vec<okytes_daemon_api::contract::PeerDevice>, String> {
    daemon_client()?
        .request(reqwest::Method::GET, "v1/peers/discovery", None::<&()>)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn peer_discovery_setting(
) -> Result<okytes_daemon_api::contract::PeerDiscoverySetting, String> {
    daemon_client()?
        .request(
            reqwest::Method::GET,
            "v1/peers/discovery/settings",
            None::<&()>,
        )
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn set_peer_discovery(
    enabled: bool,
) -> Result<okytes_daemon_api::contract::PeerDiscoverySetting, String> {
    daemon_client()?
        .request(
            reqwest::Method::POST,
            "v1/peers/discovery/settings",
            Some(&okytes_daemon_api::contract::PeerDiscoverySetting { enabled }),
        )
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn peer_requests() -> Result<Vec<okytes_daemon_api::contract::PeerRequest>, String> {
    daemon_client()?
        .request(reqwest::Method::GET, "v1/peers/requests", None::<&()>)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn peer_trust(
    input: okytes_daemon_api::contract::PeerApproval,
) -> Result<okytes_daemon_api::contract::PeerTrust, String> {
    daemon_client()?
        .request(reqwest::Method::POST, "v1/peers/trust", Some(&input))
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn peer_trusts() -> Result<Vec<okytes_daemon_api::contract::PeerTrust>, String> {
    daemon_client()?
        .request(reqwest::Method::GET, "v1/peers/trust", None::<&()>)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn peer_revoke(
    id: String,
    revision: u64,
) -> Result<okytes_daemon_api::contract::PeerTrust, String> {
    daemon_client()?
        .request::<okytes_daemon_api::contract::PeerTrust>(
            reqwest::Method::DELETE,
            &format!("v1/peers/trust/{id}/{revision}"),
            None::<&()>,
        )
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn open_peer_destination(path: String) -> Result<(), String> {
    let path = std::path::PathBuf::from(path);
    if !path.is_absolute() || !path.is_dir() {
        return Err("Transfer destination is unavailable".to_owned());
    }
    #[cfg(target_os = "windows")]
    let mut command = std::process::Command::new("explorer.exe");
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|_| "Could not open the transfer destination".to_owned())
}
#[tauri::command]
async fn peer_send(input: okytes_daemon_api::contract::PeerSend) -> Result<String, String> {
    daemon_client()?
        .request(reqwest::Method::POST, "v1/peers/send", Some(&input))
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn peer_decide(
    id: String,
    input: okytes_daemon_api::contract::PeerDecision,
) -> Result<okytes_daemon_api::contract::PeerRequest, String> {
    daemon_client()?
        .request(
            reqwest::Method::POST,
            &format!("v1/peers/requests/{id}/decision"),
            Some(&input),
        )
        .await
        .map_err(|e| e.to_string())
}

type Service = Arc<dyn AppService>;
#[tauri::command]
async fn open_quick() -> Result<okytes_daemon_api::contract::QuickPairing, String> {
    let client = okytes_daemon_api::Client::from_environment()
        .map_err(|_| "Invalid daemon configuration".to_owned())?
        .ok_or_else(|| {
            "Start the daemon with --quick and attach the desktop to it first".to_owned()
        })?;
    let pairing = client.quick_pair().await.map_err(|_| "Quick Download unavailable; enable --quick on your local daemon and use an admin-scoped native credential".to_owned())?;
    okytes_daemon_api::open_quick(&pairing.url)
        .map_err(|_| "Could not open the local page".to_owned())?;
    Ok(pairing)
}
#[tauri::command]
async fn submit(
    service: State<'_, Service>,
    input: okytes_core::desktop::Submit,
) -> Result<String, String> {
    service.submit(input).await.map_err(|e| e.to_string())
}
#[tauri::command]
async fn inspect_media(
    service: State<'_, Service>,
    url: String,
) -> Result<Vec<okytes_core::desktop::MediaChoice>, String> {
    service.inspect_media(url).await.map_err(|e| e.to_string())
}
#[tauri::command]
async fn authorize_file(
    service: State<'_, Service>,
    id: String,
    credentials: okytes_core::desktop::FileCredentials,
) -> Result<(), String> {
    service
        .authorize_file(id, credentials)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pause_all(service: State<'_, Service>) -> Result<(), String> {
    service.pause_all().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn list(service: State<'_, Service>, offset: u32) -> Result<Snapshot, String> {
    service.list(offset).await.map_err(|e| e.to_string())
}
#[tauri::command]
async fn get(service: State<'_, Service>, id: String) -> Result<Job, String> {
    service.get(id).await.map_err(|e| e.to_string())
}
#[tauri::command]
async fn telemetry(
    service: State<'_, Service>,
    id: String,
) -> Result<okytes_core::desktop::Telemetry, String> {
    service.telemetry(id).await.map_err(|e| e.to_string())
}
#[tauri::command]
fn storage_status(storage: State<'_, Arc<storage::Storage>>) -> Vec<storage::ProviderStatus> {
    storage.status()
}
#[tauri::command]
fn storage_configure(
    storage: State<'_, Arc<storage::Storage>>,
    provider: String,
    client_id: String,
    client_secret: Option<String>,
) -> Result<(), String> {
    storage.configure(&provider, client_id, client_secret)
}
#[tauri::command]
async fn storage_connect(
    storage: State<'_, Arc<storage::Storage>>,
    provider: String,
) -> Result<(), String> {
    storage.connect(&provider).await
}
#[tauri::command]
fn storage_disconnect(
    storage: State<'_, Arc<storage::Storage>>,
    provider: String,
) -> Result<(), String> {
    storage.disconnect(&provider)
}
#[tauri::command]
fn cloud_status(cloud: State<'_, Arc<cloud::Cloud>>) -> Option<cloud::CloudStatus> {
    cloud.status()
}
#[tauri::command]
async fn cloud_pair(
    cloud: State<'_, Arc<cloud::Cloud>>,
    url: String,
    code: String,
    name: String,
) -> Result<cloud::CloudStatus, String> {
    cloud.pair(url, code, name).await
}
#[tauri::command]
fn cloud_history(cloud: State<'_, Arc<cloud::Cloud>>) -> Vec<serde_json::Value> {
    cloud.remote_history()
}
#[tauri::command]
fn cloud_unpair(cloud: State<'_, Arc<cloud::Cloud>>) -> Result<(), String> {
    cloud.unpair()
}
#[tauri::command]
async fn ffmpeg_status() -> Option<String> {
    merge::ffmpeg_version().await
}
#[tauri::command]
async fn plan_merge(
    merges: State<'_, Arc<merge::Merges>>,
    parts: Vec<String>,
) -> Result<(), String> {
    merges.plan(parts).await
}
#[tauri::command]
async fn expand_playlist(service: State<'_, Service>, url: String) -> Result<Vec<String>, String> {
    service.expand_playlist(url).await.map_err(|e| e.to_string())
}
#[tauri::command]
async fn crawl(
    service: State<'_, Service>,
    input: okytes_core::desktop::CrawlInput,
) -> Result<okytes_core::desktop::CrawlResult, String> {
    service.crawl(input).await.map_err(|e| e.to_string())
}
#[tauri::command]
async fn inspect_zip(
    service: State<'_, Service>,
    url: String,
) -> Result<okytes_core::desktop::ZipListing, String> {
    service.inspect_zip(url).await.map_err(|e| e.to_string())
}
/// Open (or focus) one download's own window. The window derives its job from
/// its label, so no URL routing is needed in the bundled frontend.
#[tauri::command]
async fn open_details(app: tauri::AppHandle, id: String) -> Result<(), String> {
    // Labels allow only a narrow character set; a canonical UUID fits it.
    if id.len() != 36 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err("Invalid download ID".to_owned());
    }
    let label = format!("details-{}", id.to_ascii_lowercase());
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.unminimize();
        return window.set_focus().map_err(|e| e.to_string());
    }
    tauri::WebviewWindowBuilder::new(&app, label, tauri::WebviewUrl::App("index.html".into()))
        .title("Okytes download")
        .inner_size(560.0, 640.0)
        .min_inner_size(420.0, 420.0)
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Launch the platform opener for a completed file or its folder.
fn reveal(path: &std::path::Path, folder: bool) {
    #[cfg(target_os = "windows")]
    let result = if folder {
        std::process::Command::new("explorer.exe")
            .arg(format!("/select,{}", path.display()))
            .spawn()
    } else {
        std::process::Command::new("explorer.exe").arg(path).spawn()
    };
    #[cfg(target_os = "macos")]
    let result = if folder {
        std::process::Command::new("open").arg("-R").arg(path).spawn()
    } else {
        std::process::Command::new("open").arg(path).spawn()
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open")
        .arg(if folder {
            path.parent().unwrap_or(path)
        } else {
            path
        })
        .spawn();
    if let Err(error) = result {
        tracing::warn!(%error, "completion action could not open the download");
    }
}

#[tauri::command]
async fn add(service: State<'_, Service>, input: AddInput) -> Result<Vec<String>, String> {
    service.add(input).await.map_err(|e| e.to_string())
}
#[tauri::command]
async fn control(service: State<'_, Service>, id: String, action: Action) -> Result<(), String> {
    service.control(id, action).await.map_err(|e| e.to_string())
}
#[tauri::command]
async fn settings(service: State<'_, Service>) -> Result<Settings, String> {
    service.settings().await.map_err(|e| e.to_string())
}
#[tauri::command]
async fn set_settings(service: State<'_, Service>, settings: Settings) -> Result<(), String> {
    service
        .set_settings(settings)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn automation_state(automation: State<'_, Arc<automation::Automation>>) -> automation::View {
    automation.view()
}
#[tauri::command]
fn set_when_done(
    app: tauri::AppHandle,
    automation: State<'_, Arc<automation::Automation>>,
    action: Option<String>,
) -> Result<(), String> {
    automation.set_when_done(action)?;
    // Also ends a countdown already shown in the window.
    let _ = app.emit("when-done", None::<&str>);
    Ok(())
}
#[tauri::command]
fn set_extract(
    automation: State<'_, Arc<automation::Automation>>,
    enabled: bool,
) -> Result<(), String> {
    automation.set_extract(enabled)
}
/// Hold a download until a wall-clock time, or clear that with `at: null`.
#[tauri::command]
async fn schedule_start(
    service: State<'_, Service>,
    automation: State<'_, Arc<automation::Automation>>,
    id: String,
    at: Option<i64>,
) -> Result<(), String> {
    if at.is_none() {
        return automation.schedule(id, None);
    }
    let job = service.get(id.clone()).await.map_err(|e| e.to_string())?;
    let running = match job.state.as_str() {
        "Queued" | "Probing" | "Downloading" => true,
        "Paused" | "Failed" => false,
        _ => return Err("Only an unfinished download can be scheduled".to_owned()),
    };
    automation.schedule(id.clone(), at)?;
    if running {
        if let Err(error) = service.control(id.clone(), Action::Pause).await {
            let _ = automation.schedule(id, None);
            return Err(error.to_string());
        }
    }
    Ok(())
}

#[tauri::command]
async fn inspect_torrent(
    service: State<'_, Service>,
    input: okytes_torrent_types::Input,
) -> Result<okytes_torrent_types::Metadata, String> {
    service
        .inspect_torrent(input)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn add_torrent(
    service: State<'_, Service>,
    input: okytes_torrent_types::Input,
) -> Result<String, String> {
    service.add_torrent(input).await.map_err(|e| e.to_string())
}
#[tauri::command]
async fn torrent(
    service: State<'_, Service>,
    id: String,
) -> Result<okytes_torrent_types::Snapshot, String> {
    service.torrent(id).await.map_err(|e| e.to_string())
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(updates::Updates(tokio::sync::Mutex::new(None)))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![submit,inspect_media,authorize_file,read_clipboard,
            open_quick,
            list,
            get,
            telemetry,
            open_details,
            inspect_zip,
            crawl,
            expand_playlist,
            ffmpeg_status,
            plan_merge,
            cloud_status,
            storage_status,
            storage_configure,
            storage_connect,
            storage_disconnect,
            cloud_pair,
            cloud_unpair,
            cloud_history,
            add,
            control,
            settings,
            set_settings,
            pause_all,
            inspect_torrent,
            add_torrent,
            torrent,
            automation_state,
            set_when_done,
            set_extract,
            schedule_start,
            peer_identity,
            peer_discover,
            peer_discovery_setting,
            set_peer_discovery,
            peer_requests,
            peer_trust,
            peer_trusts,
            peer_revoke,
            open_peer_destination,
            peer_send,
            peer_decide,
            updates::check_update,
            updates::download_update,
            updates::install_update
        ])
        .setup(|app| {
            let directory = std::env::var_os("OKYTES_DATA_DIR").map(std::path::PathBuf::from).unwrap_or(app.path().app_data_dir()?);
            if !directory.is_absolute() {return Err("OKYTES_DATA_DIR must be absolute".into());}
            std::fs::create_dir_all(&directory)?;
            let cloud = cloud::Cloud::new(&directory);
            app.manage(cloud.clone());
            app.manage(Arc::new(storage::Storage::new(&directory)));
            app.manage(Arc::new(merge::Merges::new(&directory)));
            let automation = Arc::new(automation::Automation::new(&directory));
            app.manage(automation.clone());
            // Re-run on every launch so upgrades and moved portable directories
            // repair the absolute native-host manifest path automatically.
            if let Err(error) = register_native_host() {
                tracing::warn!(%error, "native host could not be registered; browser integration may be unavailable");
            }
            let destination = std::env::var_os("OKYTES_DOWNLOAD_DIR").map(std::path::PathBuf::from).unwrap_or(app.path().download_dir()?);
            let service: Service = if let Some(client)=okytes_daemon_api::Client::from_environment()? {
                Arc::new(client)
            } else {
            let session = tauri::async_runtime::block_on(Session::open(
                &directory.join("okytes.db"),
                &destination,
            ))?;
            // Registration provisions the per-user token before this listener is
            // opened. The listener owns a service reference, never a host process.
            if directory.join("browser-token.json").exists() {
                let handler: Arc<dyn okytes_browser_bridge::Handler> = session.clone();
                match tauri::async_runtime::block_on(okytes_browser_bridge::serve(&directory, handler)) {
                    Ok(listener) => { app.manage(listener); }
                    Err(_) => tracing::warn!("browser bridge could not start; browser downloads remain available"),
                }
            }
            // Cloud sync needs the embedded queue owner for idempotent capture.
            tauri::async_runtime::spawn(cloud.clone().run(session.clone()));
            session
            };
            app.manage(service.clone());
            let starts = service.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    automation.start_due(starts.as_ref()).await;
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                }
            });
            let handle = app.handle().clone();
            let notifications = service.clone();
            tauri::async_runtime::spawn(async move {
                let Ok(mut events) = notifications.subscribe().await else {
                    return;
                };
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            use okytes_core::service::EngineEvent;
                            let message = match event {
                                EngineEvent::JobCompleted { job_id } => {
                                    Some((job_id, "Download complete"))
                                }
                                EngineEvent::JobFailed { job_id, .. } => {
                                    Some((job_id, "Download failed"))
                                }
                                _ => None,
                            };
                            if let Some((id, _)) = message {
                                let merges = handle.state::<Arc<merge::Merges>>().inner().clone();
                                let (service, events) = (notifications.clone(), handle.clone());
                                // ffmpeg may run for minutes; never block event handling.
                                tauri::async_runtime::spawn(async move {
                                    let Some(result) = merges.completed(service.as_ref(), &id.to_string()).await else {
                                        return;
                                    };
                                    let (title, body) = match result {
                                        Ok(path) => ("Tracks merged", path.display().to_string()),
                                        Err(error) => {
                                            tracing::warn!(%error, "media merge failed");
                                            ("Merge failed", error)
                                        }
                                    };
                                    let _ = events.notification().builder().title(title).body(body).show();
                                });
                            }
                            if let Some((id, title)) = message {
                                let automation = handle.state::<Arc<automation::Automation>>().inner().clone();
                                let (service, events) = (notifications.clone(), handle.clone());
                                let completed = title == "Download complete";
                                // Extraction can take minutes; never block event handling.
                                tauri::async_runtime::spawn(async move {
                                    if completed {
                                        if let Ok(job) = service.get(id.to_string()).await {
                                            if let Some(result) = automation.extract(std::path::Path::new(&job.destination)).await {
                                                let (title, body) = match result {
                                                    Ok(folder) => ("Archive extracted", folder.display().to_string()),
                                                    Err(error) => {
                                                        tracing::warn!(%error, "archive extraction failed");
                                                        ("Extraction failed", format!("{}: {error}", job.name))
                                                    }
                                                };
                                                let _ = events.notification().builder().title(title).body(body).show();
                                            }
                                        }
                                    }
                                    // After extraction, so a finish action never interrupts it.
                                    automation.finished(service, events).await;
                                });
                            }
                            if let Some((id, title)) = message {
                                let Ok(mut preferences) = notifications.settings().await else {
                                    continue;
                                };
                                // A per-download completion choice runs once, then is removed.
                                // A failure keeps the choice for a later resume.
                                let key = id.to_string();
                                let completed = title == "Download complete";
                                let action = preferences.completion.as_mut().and_then(|actions| {
                                    let index = actions.iter().position(|a| a.id == key)?;
                                    if completed {
                                        Some(actions.remove(index))
                                    } else {
                                        Some(actions[index].clone())
                                    }
                                });
                                if let Some(action) = action.as_ref().filter(|_| completed) {
                                    if let Ok(job) = notifications.get(key.clone()).await {
                                        let path = std::path::Path::new(&job.destination);
                                        if action.open_file {
                                            reveal(path, false);
                                        }
                                        if action.open_folder {
                                            reveal(path, true);
                                        }
                                        if let Some(provider) = action.upload.clone() {
                                            let storage = handle.state::<Arc<storage::Storage>>().inner().clone();
                                            let (path, name, handle) = (path.to_path_buf(), job.name.clone(), handle.clone());
                                            // Uploads can take minutes; never block event handling.
                                            tauri::async_runtime::spawn(async move {
                                                let total = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                                                let (events, file, service) = (handle.clone(), name.clone(), provider.clone());
                                                let progress = move |sent: u64| {
                                                    let _ = events.emit(
                                                        "storage-progress",
                                                        serde_json::json!({ "name": file, "provider": service, "sent": sent, "total": total }),
                                                    );
                                                };
                                                let result = storage.upload(&provider, &path, &progress).await;
                                                let title = match &result {
                                                    Ok(()) => format!("Uploaded to {}", storage::label(&provider)),
                                                    Err(_) => format!("{} upload failed", storage::label(&provider)),
                                                };
                                                let body = match result {
                                                    Ok(()) => name,
                                                    Err(error) => {
                                                        tracing::warn!(%error, "storage upload failed");
                                                        format!("{name}: {error}")
                                                    }
                                                };
                                                let _ = handle.notification().builder().title(title).body(body).show();
                                            });
                                        }
                                    }
                                }
                                let notify = action.as_ref().map_or(preferences.notifications, |a| a.notify);
                                if action.is_some() && completed {
                                    if let Err(error) = notifications.set_settings(preferences).await {
                                        tracing::warn!(%error, "completion action could not be cleared");
                                    }
                                }
                                if notify
                                {
                                    if let Ok(job) = notifications.get(id.to_string()).await {
                                        if let Err(error) = handle
                                            .notification()
                                            .builder()
                                            .title(title)
                                            .body(job.name)
                                            .show() {tracing::warn!(%error,"native notification could not be delivered");}
                                    }
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });
            if let Some(peer_notifications) = okytes_daemon_api::Client::from_environment()? {
                let handle = app.handle().clone();
                let preferences = service.clone();
                tauri::async_runtime::spawn(async move {
                    let mut announced = std::collections::HashSet::new();
                    loop {
                        if preferences.settings().await.is_ok_and(|settings| settings.notifications) {
                            let requests: Result<Vec<okytes_daemon_api::contract::PeerRequest>, _> =
                                peer_notifications
                                    .request(reqwest::Method::GET, "v1/peers/requests", None::<&()>)
                                    .await;
                            if let Ok(requests) = requests {
                                for request in requests.into_iter().filter(|request| {
                                    request.direction == okytes_core::peer::Direction::Inbound
                                        && request.state == okytes_core::peer::State::AwaitingApproval
                                }) {
                                    if announced.insert(request.id) {
                                        let sender = &request.peer_fingerprint[..12];
                                        let body = format!(
                                            "Device {sender} wants to send {} item(s), {} bytes",
                                            request.manifest.entries.len(),
                                            request.total
                                        );
                                        if let Err(error) = handle
                                            .notification()
                                            .builder()
                                            .title("Incoming LAN Share")
                                            .body(body)
                                            .show()
                                        {
                                            tracing::warn!(%error,"peer notification could not be delivered");
                                        }
                                    }
                                }
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    }
                });
            }
            use tauri::{
                menu::{Menu, MenuItem},
                tray::TrayIconBuilder,
            };
            let show = MenuItem::with_id(app, "show", "Show Okytes", true, None::<&str>)?;
            let hide = MenuItem::with_id(app, "hide", "Hide window", true, None::<&str>)?;
            let pause = MenuItem::with_id(app, "pause", "Pause all", true, None::<&str>)?;
            let quick = MenuItem::with_id(app, "quick", "Open Quick Download", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &hide, &quick, &pause, &quit])?;
            let icon = tauri::image::Image::new_owned(include_bytes!("../icons/icon.rgba").to_vec(),64,64);
            TrayIconBuilder::new()
                .icon(icon)
                .menu(&menu)
                .tooltip("Okytes")
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "quick" => {
                        if let Some(window) = app.get_webview_window("main") { let _ = window.show(); let _ = window.set_focus(); }
                        let _ = app.emit("quick-open", ());
                    }
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "hide" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.hide();
                        }
                    }
                    "quit" => app.exit(0),
                    "pause" => {
                        let service = service.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(error)=service.pause_all().await {tracing::warn!(%error,"pause-all failed");}
                        });
                    }
                    _ => (),
                })
                .build(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())?;
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        tracing::error!(%error,"desktop startup failed");
        std::process::exit(1);
    }
}
