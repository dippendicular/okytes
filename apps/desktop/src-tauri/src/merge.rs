//! Audio/video merging for separately downloaded media tracks. When every job
//! in a plan has completed, ffmpeg copies the streams into one Matroska file
//! without re-encoding. Plans are a small profile file so they survive restarts.
use okytes_core::desktop::DesktopService;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const PLANS: &str = "merges.json";

#[derive(Serialize, Deserialize, Clone)]
struct Plan {
    /// Job IDs; the first provides the video and the output folder and name.
    parts: Vec<String>,
}

pub struct Merges {
    directory: PathBuf,
    lock: tokio::sync::Mutex<()>,
}

/// ffmpeg from OKYTES_FFMPEG, else from PATH.
fn program() -> PathBuf {
    std::env::var_os("OKYTES_FFMPEG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ffmpeg"))
}

fn command(program: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    // No console window flashes up for a GUI app on Windows.
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command
}

/// First line of `ffmpeg -version`, when ffmpeg can run.
pub async fn ffmpeg_version() -> Option<String> {
    let output = command(&program()).arg("-version").output().await.ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or("ffmpeg")
            .to_owned()
    })
}

fn unique(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("merged")
        .to_owned();
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("mkv")
        .to_owned();
    (2..1000)
        .map(|n| path.with_file_name(format!("{stem} ({n}).{extension}")))
        .find(|candidate| !candidate.exists())
        .unwrap_or(path)
}

impl Merges {
    pub fn new(directory: &Path) -> Self {
        Self {
            directory: directory.into(),
            lock: tokio::sync::Mutex::new(()),
        }
    }
    fn load(&self) -> Vec<Plan> {
        std::fs::read(self.directory.join(PLANS))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
    fn save(&self, plans: &[Plan]) -> Result<(), String> {
        std::fs::write(
            self.directory.join(PLANS),
            serde_json::to_vec_pretty(plans).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }

    /// Record that these jobs should be merged once all of them complete.
    pub async fn plan(&self, parts: Vec<String>) -> Result<(), String> {
        if !(2..=8).contains(&parts.len())
            || parts
                .iter()
                .any(|id| id.len() != 36 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
        {
            return Err("Select between two and eight tracks to merge".into());
        }
        let _guard = self.lock.lock().await;
        let mut plans = self.load();
        if plans.len() >= 64 {
            return Err("Too many pending merges; wait for some to finish".into());
        }
        plans.push(Plan { parts });
        self.save(&plans)
    }

    /// Called for each completed job. Returns the merged file when this
    /// completion finished a plan, or an error message if merging failed.
    pub async fn completed(
        &self,
        service: &dyn DesktopService,
        job: &str,
    ) -> Option<Result<PathBuf, String>> {
        let _guard = self.lock.lock().await;
        let mut plans = self.load();
        let index = plans
            .iter()
            .position(|p| p.parts.iter().any(|id| id == job))?;
        let mut paths = Vec::new();
        for id in &plans[index].parts {
            match service.get(id.clone()).await {
                Ok(part) if part.state == "Completed" => {
                    paths.push(PathBuf::from(part.destination))
                }
                Ok(part) if matches!(part.state.as_str(), "Failed" | "Cancelled") => {
                    plans.remove(index);
                    let _ = self.save(&plans);
                    return Some(Err(format!(
                        "{} did not complete, so nothing was merged",
                        part.name
                    )));
                }
                Ok(_) => return None, // Still waiting for another track.
                Err(_) => {
                    plans.remove(index);
                    let _ = self.save(&plans);
                    return Some(Err("A track was removed before merging".into()));
                }
            }
        }
        plans.remove(index);
        if let Err(error) = self.save(&plans) {
            return Some(Err(error));
        }
        drop(_guard);
        Some(self.run(&paths).await)
    }

    async fn run(&self, inputs: &[PathBuf]) -> Result<PathBuf, String> {
        let first = &inputs[0];
        let stem = first
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("merged");
        let output = unique(first.with_file_name(format!("{stem} (merged).mkv")));
        let mut command = command(&program());
        command.args(["-hide_banner", "-loglevel", "error", "-n"]);
        for input in inputs {
            command.arg("-i").arg(input);
        }
        // Keep every stream from every input, copied without re-encoding.
        for index in 0..inputs.len() {
            command.args(["-map", &index.to_string()]);
        }
        command.args(["-c", "copy"]).arg(&output);
        let result = command.output().await.map_err(|_| {
            "ffmpeg was not found. Install it or set OKYTES_FFMPEG to its full path.".to_owned()
        })?;
        if result.status.success() {
            Ok(output)
        } else {
            let _ = std::fs::remove_file(&output);
            let detail = String::from_utf8_lossy(&result.stderr);
            Err(format!(
                "ffmpeg could not merge the tracks: {}",
                detail.lines().last().unwrap_or("unknown error")
            ))
        }
    }
}
