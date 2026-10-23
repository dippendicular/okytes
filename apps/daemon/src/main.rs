//! Foreground daemon runner; service supervisors select the same executable.
use clap::Parser;
use okytes_core::desktop::DesktopService;
use okytes_daemon::server::{openapi, router, Api, Policy};
use std::{net::SocketAddr, path::PathBuf};
#[derive(Parser)]
struct Args {
    /// Serve the authenticated manual Quick Download page on this loopback listener.
    #[arg(long)]
    quick: bool,
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    downloads: Option<PathBuf>,
    #[arg(long, default_value = "127.0.0.1:17841")]
    bind: SocketAddr,
    /// Enable authenticated Okytes peer transfers on this address.
    #[arg(long)]
    peer_bind: Option<SocketAddr>,
    /// Friendly DNS-SD name; it is an untrusted discovery hint.
    #[arg(long, default_value = "Okytes device")]
    peer_name: String,
    /// Keep manual peer addresses enabled without multicast advertisement.
    #[arg(long)]
    no_peer_discovery: bool,
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
    if args.quick && (args.remote || args.bind.port() == 0) {
        anyhow::bail!("Quick Download requires a fixed local loopback port");
    }
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
    // Reserve the configured endpoint before opening/recovering the queue. A
    // conflicting listener must not leave a newly provisioned engine running.
    let listener = std::net::TcpListener::bind(args.bind).map_err(|_| {
        anyhow::anyhow!(
            "cannot bind {}; stop the conflicting service or choose a free --bind address",
            args.bind
        )
    })?;
    listener.set_nonblocking(true)?;
    let peer_listener = match args.peer_bind {
        Some(address) => Some(tokio::net::TcpListener::bind(address).await.map_err(|_| {
            anyhow::anyhow!("cannot bind peer listener {address}; choose another --peer-bind")
        })?),
        None => None,
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
    api.recover_peer_transfers().await?;
    if args.quick {
        api.quick.enable(&args.bind.to_string())?;
    }
    if let Some(listener) = peer_listener.as_ref() {
        api.configure_peer_discovery(
            args.peer_name.clone(),
            listener.local_addr()?.port(),
            !args.no_peer_discovery,
        )
        .await?;
    }
    if let Some(listener) = peer_listener {
        let peer_api = api.clone();
        tokio::spawn(async move {
            let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
            loop {
                let Ok((socket, address)) = listener.accept().await else {
                    break;
                };
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    drop(socket);
                    continue;
                };
                let Ok(tls) = peer_api
                    .peer_identity
                    .server_for_trusted(&peer_api.peer_store)
                    .await
                else {
                    drop(socket);
                    continue;
                };
                let Ok(settings) = peer_api.session.settings().await else {
                    drop(socket);
                    continue;
                };
                let destination = std::path::PathBuf::from(settings.destination);
                let store = peer_api.peer_store.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = okytes_daemon::peer_session::receive_socket(
                        socket,
                        tls,
                        &store,
                        &destination,
                    )
                    .await
                    {
                        tracing::warn!(%address,%error,"peer session rejected");
                    }
                });
            }
        });
    }
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
        let mut server = axum_server::from_tcp(listener)?
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
        let mut server = axum_server::from_tcp(listener)?
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
