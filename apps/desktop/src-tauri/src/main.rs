#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use okytes_core::desktop::{Action, AddInput, Job, Settings, Snapshot};
use okytes_desktop_service::{AppService, Session};
use std::sync::Arc;
use tauri::{Manager, State};
use tauri_plugin_notification::NotificationExt;
mod updates;

type Service = Arc<dyn AppService>;
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
        .invoke_handler(tauri::generate_handler![submit,inspect_media,authorize_file,
            list,
            get,
            add,
            control,
            settings,
            set_settings,
            pause_all,
            inspect_torrent,
            add_torrent,
            torrent,
            updates::check_update,
            updates::download_update,
            updates::install_update
        ])
        .setup(|app| {
            let directory = std::env::var_os("OKYTES_DATA_DIR").map(std::path::PathBuf::from).unwrap_or(app.path().app_data_dir()?);
            if !directory.is_absolute() {return Err("OKYTES_DATA_DIR must be absolute".into());}
            std::fs::create_dir_all(&directory)?;
            let destination = std::env::var_os("OKYTES_DOWNLOAD_DIR").map(std::path::PathBuf::from).unwrap_or(app.path().download_dir()?);
            let service: Service = if let Some(client)=okytes_daemon_api::Client::from_environment()? {
                Arc::new(client)
            } else {
            let session = tauri::async_runtime::block_on(Session::open(
                &directory.join("okytes.db"),
                &destination,
            ))?;
            // IPC exists only after the per-user host installer provisions its
            // token. The listener owns a service reference, never a host process.
            if directory.join("browser-token.json").exists() {
                let handler: Arc<dyn okytes_browser_bridge::Handler> = session.clone();
                match tauri::async_runtime::block_on(okytes_browser_bridge::serve(&directory, handler)) {
                    Ok(listener) => { app.manage(listener); }
                    Err(_) => tracing::warn!("browser bridge could not start; browser downloads remain available"),
                }
            }
            session
            };
            app.manage(service.clone());
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
                            if let Some((id, title)) = message {
                                if notifications
                                    .settings()
                                    .await
                                    .is_ok_and(|s| s.notifications)
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
            use tauri::{
                menu::{Menu, MenuItem},
                tray::TrayIconBuilder,
            };
            let show = MenuItem::with_id(app, "show", "Show Okytes", true, None::<&str>)?;
            let hide = MenuItem::with_id(app, "hide", "Hide window", true, None::<&str>)?;
            let pause = MenuItem::with_id(app, "pause", "Pause all", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &hide, &pause, &quit])?;
            let icon = tauri::image::Image::new_owned(include_bytes!("../icons/icon.rgba").to_vec(),64,64);
            TrayIconBuilder::new()
                .icon(icon)
                .menu(&menu)
                .tooltip("Okytes")
                .on_menu_event(move |app, event| match event.id.as_ref() {
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
