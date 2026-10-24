//! Reproducible controls: origin and worker live in separate processes. This
//! developer tool is never linked into production applications.
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use futures::StreamExt;
use okytes_core::{
    engine::{hash_file, Engine},
    service::DownloadService,
    types::{ConflictStrategy, DownloadRequest, EngineConfig},
};
use okytes_persistence::{files::LocalFiles, SqliteStore};
use okytes_test_support::server::{ServerConfig, TestServer};
use okytes_transport_http::HttpTransport;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use sysinfo::{Disks, Pid, ProcessesToUpdate, System};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Three runs per scenario/mode by default; preserve JSON and process samples.
    Run {
        #[arg(long, default_value_t = 3)]
        runs: u32,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value="both", value_parser=["fixed", "adaptive", "both"])]
        mode: String,
    },
    #[command(hide = true)]
    Worker {
        #[arg(long)]
        url: String,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        hash: String,
        #[arg(long)]
        jobs: usize,
        #[arg(long)]
        mode: String,
        #[arg(long)]
        resume: bool,
    },
}
#[derive(Serialize, Deserialize)]
struct WorkerResult {
    startup_ms: f64,
    transfer_seconds: f64,
    bytes: u64,
    completed: usize,
    jobs: usize,
    resume_correct: Option<bool>,
    resumed_checkpoint_bytes: Option<u64>,
    errors: Vec<String>,
}
#[derive(Serialize)]
struct Sample {
    elapsed_ms: u128,
    rss_bytes: u64,
    cpu_ms: u64,
}
#[derive(Serialize)]
struct Run {
    scenario: String,
    mode: String,
    iteration: u32,
    seed: u64,
    file_bytes: usize,
    origin: serde_json::Value,
    worker: WorkerResult,
    effective_bps: f64,
    sampled_peak_rss_bytes: u64,
    sampled_cpu_ms: u64,
    process_startup_ms: Option<f64>,
    peak_origin_bodies: usize,
    samples: Vec<Sample>,
}
#[derive(Serialize)]
struct Summary {
    scenario: String,
    mode: String,
    runs: usize,
    mean_bps: f64,
    stddev_bps: f64,
    coefficient_of_variation: f64,
    completion_rate: f64,
}
fn command(program: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().into())
}
fn scenarios() -> Vec<(&'static str, ServerConfig, usize, bool)> {
    let large = ServerConfig {
        file_size: 8 * 1024 * 1024,
        ..Default::default()
    };
    vec![
        (
            "single-large",
            ServerConfig {
                bandwidth_bps: Some(4 * 1024 * 1024),
                ..large.clone()
            },
            1,
            false,
        ),
        (
            "many-small",
            ServerConfig {
                file_size: 64 * 1024,
                latency: Duration::from_millis(20),
                ..Default::default()
            },
            20,
            false,
        ),
        (
            "per-connection-cap",
            ServerConfig {
                connection_bps: Some(1024 * 1024),
                ..large.clone()
            },
            1,
            false,
        ),
        (
            "high-latency",
            ServerConfig {
                latency: Duration::from_millis(60),
                bandwidth_bps: Some(4 * 1024 * 1024),
                ..large.clone()
            },
            1,
            false,
        ),
        (
            "flaky-resets",
            ServerConfig {
                disconnect_every: 7,
                bandwidth_bps: Some(4 * 1024 * 1024),
                ..large.clone()
            },
            1,
            true,
        ),
        (
            "connection-penalty",
            ServerConfig {
                max_active: Some(2),
                connection_bps: Some(2 * 1024 * 1024),
                ..large
            },
            1,
            false,
        ),
    ]
}
async fn worker(
    url: String,
    root: PathBuf,
    hash: String,
    jobs: usize,
    mode: String,
    resume: bool,
) -> Result<()> {
    let startup = Instant::now();
    let config = EngineConfig {
        database_path: root.join("jobs.sqlite"),
        min_segment_size: 64 * 1024,
        max_segment_size: 256 * 1024,
        write_buffer_size: 64 * 1024,
        max_connections_per_origin: 4,
        max_segments_per_job: 8,
        fixed_concurrency: (mode == "fixed").then_some(4),
        retry_base_delay_ms: 10,
        retry_max_delay_ms: 2000,
        ..Default::default()
    };
    let store = Arc::new(SqliteStore::open(&config.database_path)?);
    let _lease = SqliteStore::acquire_engine(&config.database_path)?;
    let create = || {
        Engine::new(
            config.clone(),
            Arc::new(HttpTransport::new(&config).map_err(anyhow::Error::from)?),
            store.clone(),
            Arc::new(LocalFiles),
        )
        .map(Arc::new)
        .map_err(anyhow::Error::from)
    };
    let mut engine = create()?;
    let mut ids = vec![];
    for index in 0..jobs {
        ids.push(
            engine
                .add(DownloadRequest {
                    url: url.parse()?,
                    output_dir: root.clone(),
                    filename: Some(format!("{index}.bin")),
                    expected_hash: Some(hash.clone()),
                    conflict_strategy: ConflictStrategy::Fail,
                    headers: vec![],
                    priority: 0,
                })
                .await?,
        );
    }
    let startup_ms = startup.elapsed().as_secs_f64() * 1000.0;
    std::fs::write(root.join("ready"), b"ready")?;
    let start = Instant::now();
    let mut resume_correct = None;
    let mut resumed_checkpoint_bytes = None;
    if resume {
        let id = ids[0];
        let running = engine.clone();
        let task = tokio::spawn(async move { running.execute_job(id).await });
        loop {
            if engine.status(id).await?.bytes_downloaded > 0 {
                break;
            }
            if task.is_finished() {
                bail!("resume fixture completed before durable progress");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        engine.pause(id).await?;
        task.await??;
        let saved = engine.status(id).await?.bytes_downloaded;
        drop(engine);
        engine = create()?;
        engine.resume(id).await?;
        resumed_checkpoint_bytes = Some(engine.status(id).await?.bytes_downloaded);
        resume_correct = Some(saved > 0 && resumed_checkpoint_bytes == Some(saved));
    }
    let outcomes = futures::stream::iter(ids.iter().copied())
        .map(|id| {
            let engine = engine.clone();
            async move { engine.execute_job(id).await }
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    let mut errors: Vec<_> = outcomes
        .into_iter()
        .filter_map(Result::err)
        .map(|e| e.to_string())
        .collect();
    let mut completed = 0;
    let mut bytes = 0;
    for id in ids {
        let job = engine.status(id).await?;
        if job.state == okytes_core::state::JobState::Completed
            && hash_file(&job.output_path).await? == hash
        {
            completed += 1;
            bytes += job.total_size.unwrap_or(0);
        } else {
            errors.push(format!("job {id} did not finish with expected SHA-256"));
        }
    }
    if resume {
        resume_correct = Some(resume_correct == Some(true) && completed == jobs);
    }
    let report = WorkerResult {
        startup_ms,
        transfer_seconds: start.elapsed().as_secs_f64(),
        bytes,
        completed,
        jobs,
        resume_correct,
        resumed_checkpoint_bytes,
        errors,
    };
    std::fs::write(
        root.join("result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(())
}
async fn run(runs: u32, output: PathBuf, mode: String) -> Result<()> {
    if !(1..=20).contains(&runs) {
        bail!("runs must be 1..20");
    }
    if output.exists() {
        bail!("use a new output path to preserve raw results");
    }
    std::fs::create_dir_all(&output)?;
    let system = System::new_all();
    let disks = Disks::new_with_refreshed_list();
    let mut source_hashes = serde_json::Map::new();
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "benchmarks/Cargo.toml",
        "benchmarks/src/main.rs",
        "crates/core/Cargo.toml",
        "crates/core/src/engine.rs",
        "crates/core/src/scheduler.rs",
        "crates/core/src/fairness.rs",
        "crates/core/src/traffic.rs",
        "crates/core/src/types.rs",
        "dev-tools/test-suite/test-support/src/server.rs",
        "crates/transport-http/src/lib.rs",
    ] {
        let bytes = std::fs::read(path)?;
        source_hashes.insert(
            path.into(),
            serde_json::Value::String(format!("{:x}", Sha256::digest(&bytes))),
        );
        let target = output.join("source").join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, bytes)?;
    }
    std::fs::write(
        output.join("source-hashes.json"),
        serde_json::to_vec_pretty(&source_hashes)?,
    )?;
    std::fs::write(
        output.join("source.patch"),
        command("git", &["diff", "HEAD"]).unwrap_or_default(),
    )?;
    let environment = serde_json::json!({"timestamp_utc":chrono::Utc::now(),"os":System::long_os_version(),"kernel":System::kernel_version(),"arch":std::env::consts::ARCH,"cpu":system.cpus().first().map(|c|c.brand()),"logical_cpus":system.cpus().len(),"memory_bytes":system.total_memory(),"disks":disks.list().iter().map(|d|serde_json::json!({"mount":d.mount_point(),"kind":format!("{:?}",d.kind()),"total_bytes":d.total_space()})).collect::<Vec<_>>(),"git_commit":command("git", &["rev-parse","HEAD"]),"git_dirty":command("git", &["status","--porcelain"]),"rustc":command("rustc", &["-Vv"]),"cargo":command("cargo", &["-V"]),"profile":"release","process_sampling_ms":20,"fixed_workers":4,"origin_and_worker":"separate processes; loopback origin runs in driver"});
    std::fs::write(
        output.join("environment.json"),
        serde_json::to_vec_pretty(&environment)?,
    )?;
    let mut environment = environment;
    environment["profile"] = serde_json::json!(if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    });
    environment["worker_temp_root"] = serde_json::json!(std::env::temp_dir());
    environment["binary_sha256"] = serde_json::json!(hash_file(&std::env::current_exe()?).await?);
    std::fs::write(
        output.join("environment.json"),
        serde_json::to_vec_pretty(&environment)?,
    )?;
    let modes = if mode == "both" {
        vec!["fixed", "adaptive"]
    } else {
        vec![mode.as_str()]
    };
    let mut records = vec![];
    for (name, config, jobs, resume) in scenarios() {
        for iteration in 0..runs {
            // Alternate order so a single mode does not always receive warm caches.
            let mut order = modes.clone();
            if iteration % 2 == 1 {
                order.reverse();
            }
            for mode in order {
                println!("{name} {mode} run {}/{}", iteration + 1, runs);
                let origin = serde_json::json!({"shared_bps":config.bandwidth_bps,"connection_bps":config.connection_bps,"latency_ms":config.latency.as_millis(),"max_active":config.max_active,"disconnect_every":config.disconnect_every,"frame_bytes":65536,"quantum_bytes":262144});
                let server = TestServer::start(config.clone()).await?;
                let root = tempfile::tempdir()?;
                let mut child = tokio::process::Command::new(std::env::current_exe()?)
                    .args([
                        "worker",
                        "--url",
                        &server.url("/normal"),
                        "--mode",
                        mode,
                        "--hash",
                        &server.content_hash,
                        "--jobs",
                        &jobs.to_string(),
                        "--root",
                    ])
                    .arg(root.path())
                    .args(if resume { vec!["--resume"] } else { vec![] })
                    .kill_on_drop(true)
                    .spawn()?;
                let pid = Pid::from_u32(child.id().context("worker PID unavailable")?);
                let mut monitor = System::new();
                let mut samples = vec![];
                let mut process_startup_ms = None;
                let start = Instant::now();
                loop {
                    if process_startup_ms.is_none() && root.path().join("ready").exists() {
                        process_startup_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
                    }
                    monitor.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
                    if let Some(process) = monitor.process(pid) {
                        samples.push(Sample {
                            elapsed_ms: start.elapsed().as_millis(),
                            rss_bytes: process.memory(),
                            cpu_ms: process.accumulated_cpu_time(),
                        });
                    }
                    if let Some(status) = child.try_wait()? {
                        if !status.success() {
                            bail!("worker failed in {name}/{mode}");
                        }
                        break;
                    }
                    if start.elapsed() > Duration::from_secs(90) {
                        child.kill().await?;
                        bail!("scenario timed out: {name}/{mode}");
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                let worker: WorkerResult =
                    serde_json::from_slice(&std::fs::read(root.path().join("result.json"))?)?;
                let record = Run {
                    scenario: name.into(),
                    mode: mode.into(),
                    iteration: iteration + 1,
                    seed: config.seed,
                    file_bytes: config.file_size,
                    origin,
                    effective_bps: worker.bytes as f64 / worker.transfer_seconds,
                    sampled_peak_rss_bytes: samples.iter().map(|s| s.rss_bytes).max().unwrap_or(0),
                    sampled_cpu_ms: samples.iter().map(|s| s.cpu_ms).max().unwrap_or(0),
                    process_startup_ms,
                    peak_origin_bodies: server.peak_connections(),
                    worker,
                    samples,
                };
                std::fs::write(
                    output.join(format!("{name}-{mode}-{}.json", iteration + 1)),
                    serde_json::to_vec_pretty(&record)?,
                )?;
                records.push(record);
                server.shutdown().await;
            }
        }
    }
    let mut summaries = vec![];
    for (name, _, _, _) in scenarios() {
        for mode in &modes {
            let selected: Vec<_> = records
                .iter()
                .filter(|r| r.scenario == name && r.mode == *mode)
                .collect();
            let n = selected.len() as f64;
            let mean = selected.iter().map(|r| r.effective_bps).sum::<f64>() / n;
            let stddev = (selected
                .iter()
                .map(|r| (r.effective_bps - mean).powi(2))
                .sum::<f64>()
                / n)
                .sqrt();
            summaries.push(Summary {
                scenario: name.into(),
                mode: (*mode).into(),
                runs: selected.len(),
                mean_bps: mean,
                stddev_bps: stddev,
                coefficient_of_variation: stddev / mean,
                completion_rate: selected.iter().map(|r| r.worker.completed).sum::<usize>() as f64
                    / selected.iter().map(|r| r.worker.jobs).sum::<usize>() as f64,
            });
        }
    }
    std::fs::write(
        output.join("summary.json"),
        serde_json::to_vec_pretty(&summaries)?,
    )?;
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    match Args::parse().command {
        Command::Run { runs, output, mode } => run(runs, output, mode).await,
        Command::Worker {
            url,
            root,
            hash,
            jobs,
            mode,
            resume,
        } => worker(url, root, hash, jobs, mode, resume).await,
    }
}
