//! Desktop automation that lives beside the queue rather than in it: one-time
//! start times, archive extraction, and an action for when every download
//! has finished. Start times and the extraction choice are a small profile
//! file. The finish action lasts for this run only, so a forgotten choice
//! never turns a computer off on a later day.
use okytes_core::desktop::{Action, DesktopService};
use okytes_desktop_service::AppService;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Duration,
};
use tauri::Emitter;
use tauri_plugin_notification::NotificationExt;

const FILE: &str = "automation.json";
const MAX_STARTS: usize = 128;
/// Time to cancel between "all downloads finished" and the action.
const GRACE_SECS: u64 = 60;
/// Archive suffixes handed to the system's tar, longest first.
const ARCHIVES: &[&str] = &[
    ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tgz", ".tbz2", ".txz", ".tar", ".zip", ".7z",
];

/// A paused download that resumes at a wall-clock time.
#[derive(Serialize, Deserialize, Clone)]
pub struct Start {
    /// Job UUID.
    id: String,
    /// Unix seconds.
    at: i64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Saved {
    #[serde(default)]
    extract: bool,
    #[serde(default)]
    starts: Vec<Start>,
}

/// What the settings and details views show.
#[derive(Serialize, Clone)]
pub struct View {
    /// sleep, shutdown or quit; null does nothing.
    when_done: Option<String>,
    /// The action now counting down, if every download has finished.
    pending: Option<String>,
    extract: bool,
    starts: Vec<Start>,
}

struct State {
    saved: Saved,
    when_done: Option<String>,
    pending: Option<String>,
}

pub struct Automation {
    directory: PathBuf,
    state: Mutex<State>,
    // Changes with every edit of the finish action; a countdown acts only if
    // it still holds the value it started with.
    generation: AtomicU64,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

fn job_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

fn tool(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command.stdin(std::process::Stdio::null()).kill_on_drop(true);
    // No console window flashes up for a GUI app on Windows.
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command
}

/// No download is waiting, connecting, transferring or being verified.
/// Paused and failed downloads do not hold the computer awake.
async fn idle(service: &dyn DesktopService) -> bool {
    let mut offset = Some(0);
    for _ in 0..256 {
        let Some(at) = offset else {
            return true;
        };
        let Ok(page) = service.list(at).await else {
            return false;
        };
        if page.jobs.iter().any(|job| {
            matches!(
                job.state.as_str(),
                "Queued" | "Probing" | "Downloading" | "Verifying"
            )
        }) {
            return false;
        }
        offset = page.next;
    }
    // More history than this loop reads: do not guess.
    offset.is_none()
}

fn power(action: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut command = if action == "shutdown" {
        let mut command = tool("shutdown.exe");
        command.args(["/s", "/t", "0"]);
        command
    } else {
        let mut command = tool("powershell.exe");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Add-Type -AssemblyName System.Windows.Forms; [void][System.Windows.Forms.Application]::SetSuspendState('Suspend', $false, $false)",
        ]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = if action == "shutdown" {
        let mut command = tool("osascript");
        command.args(["-e", "tell application \"System Events\" to shut down"]);
        command
    } else {
        let mut command = tool("pmset");
        command.arg("sleepnow");
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = tool("systemctl");
        command.arg(if action == "shutdown" {
            "poweroff"
        } else {
            "suspend"
        });
        command
    };
    // The system outlives this process; do not tie the request to it.
    command.kill_on_drop(false).spawn().map(|_| ())
}

impl Automation {
    pub fn new(directory: &Path) -> Self {
        let saved = std::fs::read(directory.join(FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
            .map(|mut saved| {
                saved.starts.retain(|start| job_id(&start.id));
                saved.starts.truncate(MAX_STARTS);
                saved
            })
            .unwrap_or_default();
        Self {
            directory: directory.into(),
            state: Mutex::new(State {
                saved,
                when_done: None,
                pending: None,
            }),
            generation: AtomicU64::new(0),
        }
    }
    fn state(&self) -> MutexGuard<'_, State> {
        // Plain data stays usable even if a holder panicked.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn save(&self, saved: &Saved) -> Result<(), String> {
        // Replace the file in one step so a crash cannot leave half of it.
        let (target, draft) = (
            self.directory.join(FILE),
            self.directory.join(format!("{FILE}.tmp")),
        );
        std::fs::write(
            &draft,
            serde_json::to_vec_pretty(saved).map_err(|e| e.to_string())?,
        )
        .and_then(|()| std::fs::rename(&draft, &target))
        .map_err(|e| format!("Could not save automation settings: {e}"))
    }

    pub fn view(&self) -> View {
        let state = self.state();
        View {
            when_done: state.when_done.clone(),
            pending: state.pending.clone(),
            extract: state.saved.extract,
            starts: state.saved.starts.clone(),
        }
    }
    /// Choose, change or cancel the finish action, including one counting down.
    pub fn set_when_done(&self, action: Option<String>) -> Result<(), String> {
        if action
            .as_deref()
            .is_some_and(|action| !matches!(action, "sleep" | "shutdown" | "quit"))
        {
            return Err("Choose sleep, shut down or quit".into());
        }
        let mut state = self.state();
        state.when_done = action;
        state.pending = None;
        self.generation.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    pub fn set_extract(&self, enabled: bool) -> Result<(), String> {
        let mut state = self.state();
        let mut saved = state.saved.clone();
        saved.extract = enabled;
        self.save(&saved)?;
        state.saved = saved;
        Ok(())
    }
    /// Set, move or (with `None`) clear one download's start time.
    pub fn schedule(&self, id: String, at: Option<i64>) -> Result<(), String> {
        if !job_id(&id) {
            return Err("Invalid download ID".into());
        }
        if at.is_some_and(|at| at <= now() || at > now() + 366 * 86_400) {
            return Err("Choose a start time within the next year".into());
        }
        let mut state = self.state();
        let mut saved = state.saved.clone();
        saved.starts.retain(|start| start.id != id);
        if let Some(at) = at {
            if saved.starts.len() >= MAX_STARTS {
                return Err("Too many scheduled downloads; clear some first".into());
            }
            saved.starts.push(Start { id, at });
        }
        self.save(&saved)?;
        state.saved = saved;
        Ok(())
    }

    /// Resume every download whose start time has come. A time that passed
    /// while the app was closed counts as due.
    pub async fn start_due(&self, service: &dyn DesktopService) {
        let due: Vec<String> = {
            let mut state = self.state();
            let time = now();
            if !state.saved.starts.iter().any(|start| start.at <= time) {
                return;
            }
            let mut saved = state.saved.clone();
            let (due, later): (Vec<Start>, Vec<Start>) = saved
                .starts
                .into_iter()
                .partition(|start| start.at <= time);
            saved.starts = later;
            if let Err(error) = self.save(&saved) {
                tracing::warn!(%error, "scheduled starts could not be saved");
            }
            state.saved = saved;
            due.into_iter().map(|start| start.id).collect()
        };
        for id in due {
            // Removed, already running or finished downloads need nothing.
            if let Err(error) = service.control(id, Action::Resume).await {
                tracing::debug!(%error, "scheduled start skipped");
            }
        }
    }

    /// Unpack a completed archive into a new folder beside it, when enabled.
    /// `None` when there is nothing to do.
    pub async fn extract(&self, file: &Path) -> Option<Result<PathBuf, String>> {
        if !self.state().saved.extract || !file.is_file() {
            return None;
        }
        let name = file.file_name()?.to_str()?;
        let suffix = ARCHIVES
            .iter()
            .find(|suffix| name.to_ascii_lowercase().ends_with(*suffix))?;
        let stem = &name[..name.len() - suffix.len()];
        if stem.is_empty() {
            return None;
        }
        // Always a new folder: extraction never writes into existing files.
        let parent = file.parent()?;
        let folder = (1..100)
            .map(|n| {
                parent.join(if n == 1 {
                    stem.to_owned()
                } else {
                    format!("{stem} ({n})")
                })
            })
            .find(|candidate| !candidate.exists())?;
        if let Err(error) = std::fs::create_dir(&folder) {
            return Some(Err(format!("Could not create {}: {error}", folder.display())));
        }
        let result = unpack(file, &folder, *suffix).await;
        if result.is_err() {
            // Only removes the folder if nothing was written into it.
            let _ = std::fs::remove_dir(&folder);
        }
        Some(result.map(|()| folder))
    }

    /// Called after a download completes or fails. When nothing is left to
    /// do, announce the chosen action and run it after a cancellable delay.
    pub async fn finished(self: Arc<Self>, service: Arc<dyn AppService>, app: tauri::AppHandle) {
        let action = {
            let state = self.state();
            if state.pending.is_some() || !state.saved.starts.is_empty() {
                return;
            }
            state.when_done.clone()
        };
        let Some(action) = action else {
            return;
        };
        if !idle(service.as_ref()).await {
            return;
        }
        let generation = {
            let mut state = self.state();
            if state.pending.is_some() || state.when_done.as_deref() != Some(action.as_str()) {
                return;
            }
            state.when_done = None;
            state.pending = Some(action.clone());
            self.generation.fetch_add(1, Ordering::SeqCst) + 1
        };
        let label = match action.as_str() {
            "shutdown" => "The computer shuts down",
            "sleep" => "The computer goes to sleep",
            _ => "Okytes quits",
        };
        let _ = app.emit("when-done", Some(&action));
        let _ = app
            .notification()
            .builder()
            .title("All downloads finished")
            .body(format!(
                "{label} in {GRACE_SECS} seconds. Open Okytes to cancel."
            ))
            .show();
        tokio::time::sleep(Duration::from_secs(GRACE_SECS)).await;
        {
            let mut state = self.state();
            if self.generation.load(Ordering::SeqCst) != generation {
                return; // Cancelled or changed; that edit already told the window.
            }
            state.pending = None;
        }
        let _ = app.emit("when-done", None::<&str>);
        // Something was added during the countdown: keep working, and keep
        // the choice for when that finishes.
        if !idle(service.as_ref()).await {
            let mut state = self.state();
            if state.when_done.is_none() && state.pending.is_none() {
                state.when_done = Some(action);
            }
            return;
        }
        if action == "quit" {
            app.exit(0);
        } else if let Err(error) = power(&action) {
            tracing::warn!(%error, "finish action could not be started");
            let _ = app
                .notification()
                .builder()
                .title("Okytes could not complete the finish action")
                .body(error.to_string())
                .show();
        }
    }
}

async fn unpack(file: &Path, folder: &Path, suffix: &str) -> Result<(), String> {
    // bsdtar (Windows 10+, macOS) reads zip, 7z and tar, strips absolute
    // paths and refuses `..` entries. Windows is addressed by its system
    // path, because a GNU tar earlier on PATH cannot read zip.
    #[cfg(target_os = "windows")]
    let program = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or("C:\\Windows".into()))
        .join("System32")
        .join("tar.exe");
    #[cfg(target_os = "macos")]
    let program = PathBuf::from("/usr/bin/tar");
    // GNU tar handles the tar family; zip and 7z need bsdtar.
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = PathBuf::from(if suffix.contains(".tar") || suffix.starts_with(".t") {
        "tar"
    } else {
        "bsdtar"
    });
    let result = tool(&program)
        .arg("-xf")
        .arg(file)
        .arg("-C")
        .arg(folder)
        .output()
        .await;
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = if result.is_err() && suffix == ".zip" {
        tool("unzip")
            .arg("-q")
            .arg("-n")
            .arg(file)
            .arg("-d")
            .arg(folder)
            .output()
            .await
    } else {
        result
    };
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    let _ = suffix;
    match result {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(format!(
            "The archive could not be extracted: {}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .last()
                .unwrap_or("unknown error")
        )),
        Err(_) => Err("No archive tool was found on this system (tar, bsdtar or unzip)".into()),
    }
}
