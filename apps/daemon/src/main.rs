//! Foreground daemon runner; service supervisors select the same executable.
use clap::Parser;
use okytes_daemon::server::{openapi, router, Api, Policy};
use std::{net::SocketAddr, path::PathBuf};
#[derive(Parser)]
struct Args {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    downloads: Option<PathBuf>,
    #[arg(long, default_value = "127.0.0.1:17841")]
    bind: SocketAddr,
    #[arg(long)]
    remote: bool,
    #[arg(long)]
    cert: Option<PathBuf>,
    #[arg(long)]
    key: Option<PathBuf>,
    #[arg(long)]
    host: Vec<String>,
    #[arg(long)]
    origin: Vec<String>,
    #[arg(long)]
    export_openapi: Option<PathBuf>,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if let Some(path) = args.export_openapi {
        std::fs::write(path, serde_json::to_vec_pretty(&openapi())?)?;
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .json()
        .init();
    if args.remote && (args.cert.is_none() || args.key.is_none() || args.host.is_empty()) {
        anyhow::bail!("remote mode requires TLS certificate, key and explicit Host allowlist");
    }
    if !args.remote
        && (!args.bind.ip().is_loopback()
            || args.cert.is_some()
            || args.key.is_some()
            || !args.host.is_empty()
            || !args.origin.is_empty())
    {
        anyhow::bail!("local mode requires loopback binding without remote options");
    }
    let directory = args
        .data_dir
        .or_else(|| std::env::var_os("OKYTES_DATA_DIR").map(PathBuf::from))
        .ok_or_else(|| anyhow::anyhow!("provide an absolute --data-dir"))?;
    let downloads = args
        .downloads
        .unwrap_or_else(|| directory.join("downloads"));
    let hosts = if args.remote {
        args.host
    } else {
        vec![args.bind.to_string()]
    };
    let api = Api::open(
        &directory,
        &downloads,
        Policy {
            hosts,
            origins: args.origin,
        },
    )
    .await?;
    let handle = axum_server::Handle::new();
    let stop = handle.clone();
    let queue = api.clone();
    tokio::spawn(async move {
        signal().await;
        if queue.shutdown().await.is_err() {
            tracing::error!("queue shutdown failed");
        }
        stop.graceful_shutdown(Some(std::time::Duration::from_secs(15)));
    });
    if args.remote {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = args
            .cert
            .ok_or_else(|| anyhow::anyhow!("missing certificate"))?;
        let key = args.key.ok_or_else(|| anyhow::anyhow!("missing key"))?;
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
        let acceptor = axum_server::tls_rustls::RustlsAcceptor::new(tls)
            .handshake_timeout(std::time::Duration::from_secs(5))
            .acceptor(okytes_daemon::transport::Gate::new(64));
        let mut server = axum_server::bind(args.bind)
            .acceptor(acceptor)
            .handle(handle);
        server
            .http_builder()
            .http1()
            .timer(hyper_util::rt::TokioTimer::new())
            .header_read_timeout(std::time::Duration::from_secs(5))
            .max_headers(32)
            .max_buf_size(32768);
        server.serve(router(api).into_make_service()).await?;
    } else {
        let mut server = axum_server::bind(args.bind)
            .acceptor(okytes_daemon::transport::Gate::new(64))
            .handle(handle);
        server
            .http_builder()
            .http1()
            .timer(hyper_util::rt::TokioTimer::new())
            .header_read_timeout(std::time::Duration::from_secs(5))
            .max_headers(32)
            .max_buf_size(32768);
        server.serve(router(api).into_make_service()).await?;
    }
    Ok(())
}
async fn signal() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
