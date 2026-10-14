//! Foreground CLI using the same engine and checkpoint store as library clients.
use anyhow::{bail, Result};
use base64::Engine as _;
use clap::{Parser, Subcommand};
use futures::{stream, StreamExt};
use okytes_core::{
    engine::Engine,
    service::DownloadService,
    state::JobState,
    types::{ConflictStrategy, DownloadRequest, EngineConfig},
};
use okytes_persistence::SqliteStore;
use okytes_torrent_types::TorrentService;
use okytes_transport_http::HttpTransport;
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "okytes",
    version,
    about = "Crash-safe downloads, torrents and magnet links"
)]
struct Args {
    #[arg(long, global = true)]
    daemon_url: Option<String>,
    #[arg(long, global = true)]
    token_file: Option<PathBuf>,
    #[arg(long, global = true, default_value = "okytes.db")]
    db: PathBuf,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Add a magnet or .torrent. JSON options configure selection, privacy and seeding.
    Torrent {
        source: String,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        options: Option<PathBuf>,
        #[arg(long)]
        run: bool,
    },
    /// Read peers, metadata and verified pieces.
    TorrentStatus {
        id: Uuid,
    },
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
    /// Read or replace persisted bandwidth and recurring schedule policy.
    Traffic {
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
    let remote = match (&args.daemon_url, &args.token_file) {
        (None, None) => okytes_daemon_api::Client::from_environment()?,
        (Some(url), Some(file)) => Some(okytes_daemon_api::Client::from_file(url.clone(), file)?),
        _ => bail!("provide both --daemon-url and --token-file"),
    };
    if let Some(client) = remote {
        return remote_execute(args, client).await;
    }
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
    let torrent = okytes_transport_torrent::Adapter::new(
        store.clone(),
        std::path::absolute(&args.db)?.with_extension("torrent-data"),
        std::path::absolute(&config.default_output_dir)?,
        engine.traffic_controller(),
        config.traffic.clone(),
        engine.event_sender(),
    )?;
    match args.command {
        Command::Torrent {
            source,
            output,
            options,
            run,
        } => {
            let input = torrent_input(source, output, options)?;
            let id: Uuid = torrent.add_torrent(input).await?.parse()?;
            print(id, args.json)?;
            if run {
                let _lease = Arc::new(SqliteStore::acquire_engine(&args.db)?);
                torrent.retain_lease(_lease.clone())?;
                torrent.execute_job(id).await?;
                print(torrent.status(id).await?, args.json)?;
            }
        }
        Command::TorrentStatus { id } => print(torrent.torrent(id.to_string()).await?, args.json)?,
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
                let _lease = Arc::new(SqliteStore::acquire_engine(&args.db)?);
                torrent.retain_lease(_lease.clone())?;
                execute_job(&engine, &torrent, id).await?;
                print(engine.status(id).await?, args.json)?;
            }
        }
        Command::List { offset } => print(store.page(offset).await?, args.json)?,
        Command::Status { id } => print(engine.status(id).await?, args.json)?,
        Command::Pause { id } => {
            if engine.status(id).await?.protocol == "torrent" {
                torrent.pause(id).await?;
            } else {
                engine.pause(id).await?;
            }
            print(engine.status(id).await?, args.json)?;
        }
        Command::Resume { id, run } => {
            if engine.status(id).await?.protocol == "torrent" {
                torrent.resume(id).await?;
            } else {
                engine.resume(id).await?;
            }
            if run {
                let _lease = Arc::new(SqliteStore::acquire_engine(&args.db)?);
                torrent.retain_lease(_lease.clone())?;
                execute_job(&engine, &torrent, id).await?;
            }
            print(engine.status(id).await?, args.json)?;
        }
        Command::Cancel { id } => {
            if engine.status(id).await?.protocol == "torrent" {
                torrent.cancel(id).await?;
            } else {
                engine.cancel(id).await?;
            }
            if engine.status(id).await?.protocol == "http" {
                if let Ok(_lease) = SqliteStore::acquire_engine(&args.db) {
                    execute_job(&engine, &torrent, id).await?;
                }
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
        Command::Traffic { set } => {
            let mut config = store.config().await?;
            if let Some(path) = set {
                config.traffic = serde_json::from_slice(&std::fs::read(path)?)?;
                config.validate()?;
                store.set_config(&config).await?;
            }
            print(config.traffic, args.json)?;
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
            let _lease = Arc::new(SqliteStore::acquire_engine(&args.db)?);
            torrent.retain_lease(_lease.clone())?;
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
                        let torrent = torrent.clone();
                        async move { (id, execute_job(&engine, &torrent, id).await) }
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

async fn remote_record(
    client: &okytes_daemon_api::Client,
    id: Uuid,
) -> Result<okytes_core::types::JobRecord> {
    Ok(client
        .request(
            reqwest::Method::GET,
            &format!("v1/records/{id}"),
            None::<&()>,
        )
        .await?)
}
async fn remote_wait(
    client: &okytes_daemon_api::Client,
    id: Uuid,
) -> Result<okytes_core::types::JobRecord> {
    loop {
        let record = remote_record(client, id).await?;
        if record.state == JobState::Failed {
            bail!("download failed; inspect status");
        }
        if record.state.is_terminal() || record.state == JobState::Paused {
            return Ok(record);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}
async fn remote_execute(args: Args, client: okytes_daemon_api::Client) -> Result<()> {
    use okytes_core::desktop::{Action, DesktopService, Submit};
    use reqwest::Method;
    match args.command {
        Command::Torrent {
            source,
            output,
            options,
            run,
        } => {
            let id: Uuid = client
                .add_torrent(torrent_input(source, output, options)?)
                .await?
                .parse()?;
            print(id, args.json)?;
            if run {
                print(remote_wait(&client, id).await?, args.json)?;
            }
        }
        Command::TorrentStatus { id } => print(client.torrent(id.to_string()).await?, args.json)?,
        Command::Add {
            url,
            output,
            filename,
            sha256,
            run,
            conflict,
        } => {
            let destination = output
                .map(|path| {
                    if path.is_absolute() {
                        Ok(path)
                    } else {
                        std::env::current_dir().map(|cwd| cwd.join(path))
                    }
                })
                .transpose()?
                .map(|p| p.to_string_lossy().into_owned());
            let id = client
                .submit(Submit {
                    url,
                    destination,
                    filename,
                    sha256,
                    conflict,
                })
                .await?;
            let id = Uuid::parse_str(&id)?;
            print(id, args.json)?;
            if run {
                print(remote_wait(&client, id).await?, args.json)?;
            }
        }
        Command::List { offset } => {
            let records: Vec<okytes_core::types::JobRecord> = client
                .request(
                    Method::GET,
                    &format!("v1/records?offset={offset}"),
                    None::<&()>,
                )
                .await?;
            print(records, args.json)?;
        }
        Command::Status { id } => print(remote_record(&client, id).await?, args.json)?,
        Command::Pause { id } => {
            client.control(id.to_string(), Action::Pause).await?;
            print(remote_record(&client, id).await?, args.json)?;
        }
        Command::Resume { id, run } => {
            client.control(id.to_string(), Action::Resume).await?;
            print(
                if run {
                    remote_wait(&client, id).await?
                } else {
                    remote_record(&client, id).await?
                },
                args.json,
            )?;
        }
        Command::Cancel { id } => {
            client.control(id.to_string(), Action::Cancel).await?;
            print(remote_record(&client, id).await?, args.json)?;
        }
        Command::Queue { id, priority } => {
            client
                .control(id.to_string(), Action::Priority(priority))
                .await?;
            print(remote_record(&client, id).await?, args.json)?;
        }
        Command::Traffic { set } => {
            let mut config: EngineConfig = client
                .request(Method::GET, "v1/config", None::<&()>)
                .await?;
            if let Some(path) = set {
                config.traffic = serde_json::from_slice(&std::fs::read(path)?)?;
                config.validate()?;
                client
                    .request::<()>(Method::PUT, "v1/config", Some(&config))
                    .await?;
            }
            print(config.traffic, args.json)?;
        }
        Command::Config { set } => {
            if let Some(path) = set {
                let config: EngineConfig = serde_json::from_slice(&std::fs::read(path)?)?;
                client
                    .request::<()>(Method::PUT, "v1/config", Some(&config))
                    .await?;
            }
            let config: EngineConfig = client
                .request(Method::GET, "v1/config", None::<&()>)
                .await?;
            print(config, args.json)?;
        }
        Command::Run => {
            let mut offset = 0;
            loop {
                let records: Vec<okytes_core::types::JobRecord> = client
                    .request(
                        Method::GET,
                        &format!("v1/records?offset={offset}"),
                        None::<&()>,
                    )
                    .await?;
                let count = records.len();
                for record in records {
                    if record.state == JobState::Queued || record.state.is_active() {
                        print(remote_wait(&client, record.id).await?, args.json)?;
                    }
                }
                if count < 256 {
                    break;
                }
                offset += count;
            }
        }
    }
    Ok(())
}

fn torrent_input(
    source: String,
    output: Option<PathBuf>,
    options: Option<PathBuf>,
) -> Result<okytes_torrent_types::Input> {
    let options = match options {
        Some(path) => serde_json::from_slice(&read_bounded(&path, 65536)?)?,
        None => Default::default(),
    };
    let (magnet, metainfo) = if source.starts_with("magnet:") {
        (Some(source), None)
    } else {
        let path = PathBuf::from(source);
        if !std::fs::metadata(&path)?.is_file() || std::fs::metadata(&path)?.len() > 512 * 1024 {
            bail!("metainfo exceeds 512 KiB");
        }
        (
            None,
            Some(
                base64::engine::general_purpose::STANDARD.encode(read_bounded(&path, 512 * 1024)?),
            ),
        )
    };
    Ok(okytes_torrent_types::Input {
        magnet,
        metainfo,
        destination: output
            .map(std::path::absolute)
            .transpose()?
            .map(|p| p.to_string_lossy().into_owned()),
        options,
    })
}
async fn execute_job(
    engine: &Engine,
    torrent: &okytes_transport_torrent::Adapter,
    id: Uuid,
) -> Result<()> {
    if engine.status(id).await?.protocol == "torrent" {
        torrent.execute_job(id).await?;
    } else {
        engine.execute_job(id).await?;
    }
    Ok(())
}

fn read_bounded(path: &std::path::Path, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = vec![];
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("torrent input exceeds its size limit");
    }
    Ok(bytes)
}
