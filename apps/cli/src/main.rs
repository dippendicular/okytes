//! Foreground CLI using the same engine and checkpoint store as library clients.
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use futures::{stream, StreamExt};
use okytes_core::{
    engine::Engine,
    service::DownloadService,
    state::JobState,
    types::{ConflictStrategy, DownloadRequest, EngineConfig},
};
use okytes_persistence::SqliteStore;
use okytes_transport_http::HttpTransport;
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "okytes", version, about = "Crash-safe HTTP download manager")]
struct Args {
    #[arg(long, global = true, default_value = "okytes.db")]
    db: PathBuf,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Add a job; use --run to download it immediately in this foreground process.
    Add {
        url: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long)]
        filename: Option<String>,
        #[arg(long)]
        sha256: Option<String>,
        #[arg(long)]
        run: bool,
        #[arg(long,default_value="rename",value_parser=["rename","fail","overwrite"])]
        conflict: String,
    },
    /// Print a bounded page (256 jobs) in priority order.
    List {
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    Status {
        id: Uuid,
    },
    Pause {
        id: Uuid,
    },
    Resume {
        id: Uuid,
        #[arg(long)]
        run: bool,
    },
    Cancel {
        id: Uuid,
    },
    /// Run all queued and crash-interrupted jobs with bounded concurrency.
    Run,
    /// Change the priority of a queued job (higher values run first).
    Queue {
        id: Uuid,
        priority: i32,
    },
    /// Show or replace validated engine configuration from a JSON file.
    Config {
        #[arg(long)]
        set: Option<PathBuf>,
    },
}

fn validate(c: &EngineConfig) -> Result<()> {
    c.validate()?;
    Ok(())
}
fn print(value: impl serde::Serialize, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(&value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let json = args.json;
    if let Err(error) = execute(args).await {
        if json {
            eprintln!("{}", serde_json::json!({"error":error.to_string()}));
            std::process::exit(1);
        }
        return Err(error);
    }
    Ok(())
}

async fn execute(args: Args) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let store = Arc::new(SqliteStore::open(&args.db)?);
    let mut config = store.config().await?;
    config.database_path = args.db.clone();
    validate(&config)?;
    let engine = Arc::new(Engine::new(
        config.clone(),
        Arc::new(HttpTransport::new(&config)?),
        store.clone(),
        Arc::new(okytes_persistence::files::LocalFiles),
    )?);
    match args.command {
        Command::Add {
            url,
            output,
            filename,
            sha256,
            run,
            conflict,
        } => {
            let url = url.parse()?;
            let id = engine
                .add(DownloadRequest {
                    url,
                    output_dir: output.unwrap_or(config.default_output_dir),
                    filename,
                    expected_hash: sha256,
                    conflict_strategy: match conflict.as_str() {
                        "fail" => ConflictStrategy::Fail,
                        "overwrite" => ConflictStrategy::Overwrite,
                        _ => ConflictStrategy::Rename,
                    },
                    headers: vec![],
                    priority: 0,
                })
                .await?;
            print(id, args.json)?;
            if run {
                let _lease = SqliteStore::acquire_engine(&args.db)?;
                engine.execute_job(id).await?;
                print(engine.status(id).await?, args.json)?;
            }
        }
        Command::List { offset } => print(store.page(offset).await?, args.json)?,
        Command::Status { id } => print(engine.status(id).await?, args.json)?,
        Command::Pause { id } => {
            engine.pause(id).await?;
            print(engine.status(id).await?, args.json)?;
        }
        Command::Resume { id, run } => {
            engine.resume(id).await?;
            if run {
                let _lease = SqliteStore::acquire_engine(&args.db)?;
                engine.execute_job(id).await?;
            }
            print(engine.status(id).await?, args.json)?;
        }
        Command::Cancel { id } => {
            engine.cancel(id).await?;
            if let Ok(_lease) = SqliteStore::acquire_engine(&args.db) {
                engine.execute_job(id).await?;
            }
            print(engine.status(id).await?, args.json)?;
        }
        Command::Queue { id, priority } => {
            use okytes_core::engine::JobStore;
            let mut job = engine.status(id).await?;
            if job.state != JobState::Queued {
                bail!("only queued jobs can be reprioritized");
            }
            job.priority = priority;
            store.save_job(&job).await?;
            print(job, args.json)?;
        }
        Command::Config { set } => {
            if let Some(path) = set {
                let config: EngineConfig = serde_json::from_slice(&std::fs::read(path)?)?;
                validate(&config)?;
                store.set_config(&config).await?;
            }
            print(store.config().await?, args.json)?;
        }
        Command::Run => {
            let _lease = SqliteStore::acquire_engine(&args.db)?;
            let mut offset = 0;
            let mut failed = false;
            loop {
                let page = store.page(offset).await?;
                let count = page.len();
                if count == 0 {
                    break;
                }
                let ids = page
                    .into_iter()
                    .filter(|j| j.state == JobState::Queued || j.state.is_active())
                    .map(|j| j.id);
                let mut results = stream::iter(ids)
                    .map(|id| {
                        let engine = engine.clone();
                        async move { (id, engine.execute_job(id).await) }
                    })
                    .buffer_unordered(config.max_concurrent_jobs);
                while let Some((id, result)) = results.next().await {
                    match result {
                        Ok(()) => print(engine.status(id).await?, args.json)?,
                        Err(e) => {
                            failed = true;
                            print(
                                serde_json::json!({"id":id,"error":e.to_string()}),
                                args.json,
                            )?;
                        }
                    }
                }
                offset += count;
            }
            if failed {
                bail!("one or more jobs failed; inspect status and retry with resume");
            }
        }
    }
    Ok(())
}
