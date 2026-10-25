//! Authenticated v1 API; every route, including errors and the contract, passes
//! the same Host/Origin, authentication, scope and resource admission checks.
use crate::auth::Tokens;
use axum::{
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures::{stream, Stream, StreamExt};
use okytes_core::desktop::DesktopService;
use okytes_daemon_api::contract::*;
use okytes_desktop_service::Session;
use okytes_persistence::SqliteStore;
use okytes_torrent_types::{
    Input as TorrentInput, Metadata as TorrentMetadata, Snapshot as TorrentSnapshot, TorrentService,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    convert::Infallible,
    io,
    path::Path as FsPath,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};
use utoipa::OpenApi;

/// Fixed listener policy. Host authorities and browser origins are exact values.
#[derive(Clone)]
pub struct Policy {
    /// Explicit HTTP Host allowlist, including ports.
    pub hosts: Vec<String>,
    /// Exact permitted browser origins; empty rejects all.
    pub origins: Vec<String>,
}
struct Rate {
    since: Instant,
    total: u32,
    tokens: HashMap<String, u32>,
}
/// Shared state; disconnecting request handlers never owns engine tasks.
pub struct Api {
    /// Explicitly configured loopback browser surface.
    pub quick: crate::quick::Quick,
    stopping: std::sync::atomic::AtomicBool,
    /// Real embedded queue owner used by daemon and desktop.
    pub session: Arc<Session>,
    /// Revocable credential registry.
    pub tokens: Arc<Tokens>,
    /// Peer metadata shares the session's serialized profile writer.
    pub peer_store: Arc<SqliteStore>,
    /// Protected local credential; only its public certificate is exported.
    pub peer_identity: Arc<crate::peer_identity::Identity>,
    /// Optional standards-based LAN discovery, disabled by configuration.
    pub discovery: tokio::sync::RwLock<Option<Arc<crate::discovery::Discovery>>>,
    discovery_config: tokio::sync::RwLock<Option<(String, u16)>>,
    policy: Policy,
    rate: Mutex<Rate>,
    requests: Arc<Semaphore>,
    subscribers: Arc<Semaphore>,
    peer_tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    idempotency: Mutex<()>,
    event_cursor: std::sync::atomic::AtomicU64,
}
impl Api {
    /// Open the real service and separate hashed credential registry.
    pub async fn open(
        directory: &FsPath,
        destination: &FsPath,
        policy: Policy,
    ) -> io::Result<Arc<Self>> {
        if !directory.is_absolute()
            || !destination.is_absolute()
            || policy.hosts.is_empty()
            || policy
                .hosts
                .iter()
                .any(|h| h.is_empty() || h.contains(['/', '\\', '*']))
            || policy
                .origins
                .iter()
                .any(|o| o.contains('*') || !o.starts_with("https://"))
        {
            return Err(io::Error::other("invalid daemon policy"));
        }
        std::fs::create_dir_all(directory)?;
        std::fs::create_dir_all(destination)?;
        let database = directory.join("okytes.db");
        let session = Session::open(&database, destination)
            .await
            .map_err(|_| io::Error::other("cannot open queue profile"))?;
        let peer_store = session.profile_store();
        let peer_identity = Arc::new(crate::peer_identity::Identity::open(directory)?);
        let tokens = Tokens::open(
            Arc::new(
                SqliteStore::open(&database)
                    .map_err(|_| io::Error::other("cannot open token store"))?,
            ),
            directory,
        )
        .await?;
        Ok(Arc::new(Self {
            quick: crate::quick::Quick::default(),
            stopping: std::sync::atomic::AtomicBool::new(false),
            session,
            tokens,
            peer_store,
            peer_identity,
            discovery: tokio::sync::RwLock::new(None),
            discovery_config: tokio::sync::RwLock::new(None),
            policy,
            rate: Mutex::new(Rate {
                since: Instant::now(),
                total: 0,
                tokens: HashMap::new(),
            }),
            requests: Arc::new(Semaphore::new(32)),
            subscribers: Arc::new(Semaphore::new(16)),
            peer_tasks: Mutex::new(Vec::new()),
            idempotency: Mutex::new(()),
            event_cursor: std::sync::atomic::AtomicU64::new(1),
        }))
    }
    /// Reject new requests before draining durable engine writes.
    pub async fn shutdown(&self) -> io::Result<()> {
        self.stopping
            .store(true, std::sync::atomic::Ordering::Release);
        for task in self.peer_tasks.lock().await.drain(..) {
            task.abort();
        }
        self.session
            .shutdown()
            .await
            .map_err(|_| io::Error::other("queue shutdown failed"))
    }

    /// Restart locally authorized outbound transfers after process recovery.
    pub async fn recover_peer_transfers(self: &Arc<Self>) -> io::Result<()> {
        let outbounds = self
            .peer_store
            .peer_outbounds()
            .await
            .map_err(|_| io::Error::other("cannot read peer recovery records"))?;
        for outbound in outbounds {
            let request = self
                .peer_store
                .peer_request(outbound.request_id)
                .await
                .map_err(|_| io::Error::other("cannot read peer request"))?;
            if request.is_some_and(|record| {
                !matches!(
                    record.state,
                    okytes_core::peer::State::Completed
                        | okytes_core::peer::State::Cancelled
                        | okytes_core::peer::State::Rejected
                        | okytes_core::peer::State::Paused
                )
            }) {
                launch_outbound(self.clone(), outbound)
                    .await
                    .map_err(|_| io::Error::other("cannot recover peer transfer"))?;
            }
        }
        Ok(())
    }

    pub async fn configure_peer_discovery(
        &self,
        name: String,
        port: u16,
        enabled: bool,
    ) -> io::Result<()> {
        *self.discovery_config.write().await = Some((name, port));
        self.set_peer_discovery(enabled).await
    }

    async fn set_peer_discovery(&self, enabled: bool) -> io::Result<()> {
        let mut discovery = self.discovery.write().await;
        if enabled && discovery.is_none() {
            let (name, port) = self
                .discovery_config
                .read()
                .await
                .clone()
                .ok_or_else(|| io::Error::other("peer listener is not configured"))?;
            *discovery = Some(Arc::new(crate::discovery::Discovery::start(
                &name,
                port,
                self.peer_identity.fingerprint().as_str(),
            )?));
        } else if !enabled {
            *discovery = None;
        }
        Ok(())
    }
}
fn error(status: StatusCode, code: &str) -> Response {
    let (category, retryable, message) = match status {
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            (ErrorCategory::Validation, false, "The request is invalid.")
        }
        StatusCode::UNAUTHORIZED => (
            ErrorCategory::Authentication,
            false,
            "Authentication is required.",
        ),
        StatusCode::FORBIDDEN => (
            ErrorCategory::Authorization,
            false,
            "The credential does not permit this operation.",
        ),
        StatusCode::CONFLICT => (
            ErrorCategory::Conflict,
            false,
            "The request conflicts with current state.",
        ),
        StatusCode::NOT_FOUND => (
            ErrorCategory::NotFound,
            false,
            "The requested resource was not found.",
        ),
        StatusCode::TOO_MANY_REQUESTS => (
            ErrorCategory::RateLimit,
            true,
            "The service is busy; retry later.",
        ),
        StatusCode::REQUEST_TIMEOUT => (ErrorCategory::Timeout, true, "The operation timed out."),
        StatusCode::SERVICE_UNAVAILABLE => (
            ErrorCategory::Unavailable,
            true,
            "The service is unavailable.",
        ),
        _ => (ErrorCategory::Internal, false, "The operation failed."),
    };
    (
        status,
        Json(ApiError {
            error: code.into(),
            category,
            retryable,
            message: message.into(),
        }),
    )
        .into_response()
}
fn rejection(_: impl std::fmt::Display) -> Response {
    error(StatusCode::BAD_REQUEST, "invalid_request")
}
async fn destination(api: &Api, scope: Scope, value: Option<&str>) -> Result<(), Response> {
    let Some(value) = value else { return Ok(()) };
    let path = FsPath::new(value);
    if value.len() > 4096
        || !path.is_absolute()
        || path
            .components()
            .any(|part| part == std::path::Component::ParentDir)
    {
        return Err(rejection("unsafe destination"));
    }
    if scope == Scope::Admin {
        return Ok(());
    }
    let root = std::fs::canonicalize(api.session.settings().await.map_err(rejection)?.destination)
        .map_err(rejection)?;
    let mut existing = path.to_path_buf();
    while !existing.exists() {
        if !existing.pop() {
            return Err(rejection("invalid destination"));
        }
    }
    if !std::fs::canonicalize(existing)
        .map_err(rejection)?
        .starts_with(root)
    {
        return Err(rejection("destination outside configured root"));
    }
    Ok(())
}
fn required(method: &Method, path: &str) -> Scope {
    if path.starts_with("/v1/tokens")
        || path.starts_with("/v1/peers/trust")
        || path == "/v1/peers/identity"
        || path == "/v1/quick/pair"
        || (*method == Method::PUT && matches!(path, "/v1/settings" | "/v1/config"))
    {
        Scope::Admin
    } else if *method == Method::GET {
        Scope::ReadOnly
    } else {
        Scope::Control
    }
}
async fn guard(State(api): State<Arc<Api>>, mut request: Request, next: Next) -> Response {
    if api.stopping.load(std::sync::atomic::Ordering::Acquire) {
        return error(StatusCode::SERVICE_UNAVAILABLE, "shutting_down");
    }
    let host = request
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            request
                .uri()
                .authority()
                .map(|authority| authority.as_str())
        });
    if !host.is_some_and(|host| {
        api.policy
            .hosts
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
    }) {
        return error(StatusCode::FORBIDDEN, "invalid_host");
    }
    if let Some(origin) = request.headers().get("origin") {
        if !origin.to_str().ok().is_some_and(|origin| {
            api.policy.origins.iter().any(|allowed| allowed == origin)
                || api.quick.origin_allowed(origin)
        }) {
            return error(StatusCode::FORBIDDEN, "invalid_origin");
        }
    }
    let allowed = {
        let mut rate = api.rate.lock().await;
        if rate.since.elapsed() >= Duration::from_secs(60) {
            rate.since = Instant::now();
            rate.total = 0;
            rate.tokens.clear();
        }
        rate.total = rate.total.saturating_add(1);
        rate.total <= 1200
    };
    if !allowed {
        return error(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    if request.uri().path() == "/quick" || request.uri().path().starts_with("/quick/") {
        return crate::quick::public(&api, request).await;
    }
    if !request.headers().contains_key("authorization") && request.headers().contains_key("cookie")
    {
        let Some((token, _)) = api.quick.authenticate(request.headers(), true).await else {
            return error(StatusCode::UNAUTHORIZED, "unauthorized");
        };
        request = match crate::quick::browser_request(request).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let Ok(value) = format!("Bearer {token}").parse() else {
            return error(StatusCode::UNAUTHORIZED, "unauthorized");
        };
        request.headers_mut().insert("authorization", value);
    }
    let Some(token) = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    let Some(info) = api.tokens.authenticate(token).await else {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    let scope = required(request.method(), request.uri().path());
    if info.scope < scope {
        return error(StatusCode::FORBIDDEN, "insufficient_scope");
    }
    if request
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|size| size > 1048576)
    {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large");
    }
    let allowed = {
        let mut rate = api.rate.lock().await;
        let count = rate.tokens.entry(info.id.clone()).or_default();
        *count = count.saturating_add(1);
        *count <= 600
    };
    if !allowed {
        return error(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    let Ok(_permit) = api.requests.clone().try_acquire_owned() else {
        return error(StatusCode::TOO_MANY_REQUESTS, "busy");
    };
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|route| route.as_str().to_owned())
        .unwrap_or_else(|| "unmatched".into());
    request.extensions_mut().insert(info.clone());
    let mut result = match tokio::time::timeout(Duration::from_secs(8), next.run(request)).await {
        Ok(response) => response,
        Err(_) => error(StatusCode::REQUEST_TIMEOUT, "request_timeout"),
    };
    if result.status().is_client_error() {
        let status = result.status();
        let code = match status.as_u16() {
            400 | 422 => "invalid_request",
            404 => "not_found",
            405 => "method_not_allowed",
            408 => "request_timeout",
            413 => "body_too_large",
            415 => "invalid_content_type",
            429 => "rate_limited",
            _ => "request_rejected",
        };
        result = error(status, code);
    }
    tracing::info!(token_id=%info.id,?method,route=%route,status=result.status().as_u16(),"daemon request");
    result
}

/// Construct the complete authenticated router.
pub fn router(api: Arc<Api>) -> Router {
    Router::new()
        .route("/quick", get(|| async { StatusCode::NOT_FOUND }))
        .route("/quick/{*path}", axum::routing::any(|| async { StatusCode::NOT_FOUND }))
        .route("/v1/quick/pair", post(quick_pair))
        .route("/v1/jobs", get(list).post(add))
        .route("/v1/torrents", post(add_torrent))
        .route("/v1/torrents/inspect", post(inspect_torrent))
        .route("/v1/torrents/{id}", get(torrent))
        .route("/v1/jobs/{id}", get(job))
        .route("/v1/jobs/{id}/control", post(control))
        .route("/v1/submit", post(submit))
        .route("/v1/media/inspect", post(inspect_media))
        .route("/v1/jobs/{id}/credentials", post(authorize_file))
        .route("/v1/records", get(records))
        .route("/v1/records/{id}", get(record))
        .route("/v1/config", get(config).put(set_config))
        .route("/v1/pause-all", post(pause_all))
        .route("/v1/settings", get(settings).put(set_settings))
        .route("/v1/capture", post(capture))
        .route("/v1/events", get(events))
        .route("/v1/tokens", get(tokens).post(issue))
        .route("/v1/tokens/{id}", axum::routing::delete(revoke))
        .route("/v1/peers/identity", get(peer_identity))
        .route("/v1/peers/trust", get(peer_trusts).post(peer_approve))
        .route("/v1/peers/trust/{id}/{revision}", axum::routing::delete(peer_revoke))
        .route("/v1/peers/requests", get(peer_requests))
        .route("/v1/peers/send", post(peer_send))
        .route("/v1/peers/discovery", get(peer_devices))
        .route(
            "/v1/peers/discovery/settings",
            get(peer_discovery_setting).post(peer_set_discovery),
        )
        .route("/v1/peers/requests/{id}/decision", post(peer_decide))
        .route("/v1/openapi.json", get(spec))
        .route("/v1/capabilities", get(capabilities))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not_found") })
        .layer(DefaultBodyLimit::max(1048576))
        .layer(middleware::from_fn_with_state(api.clone(), guard))
        .layer(middleware::from_fn(|request: Request, next: Next| async move {
            crate::quick::secure(next.run(request).await)
        }))
        .with_state(api)
}

#[utoipa::path(get,path="/v1/peers/identity",responses((status=200,body=PeerIdentity)),security(("bearer"=[])))]
async fn peer_identity(State(api): State<Arc<Api>>) -> Json<PeerIdentity> {
    Json(PeerIdentity {
        fingerprint: api.peer_identity.fingerprint().as_str().to_owned(),
        certificate: BASE64.encode(api.peer_identity.certificate()),
    })
}

#[utoipa::path(get,path="/v1/peers/trust",responses((status=200,body=[PeerTrust])),security(("bearer"=[])))]
async fn peer_trusts(State(api): State<Arc<Api>>) -> Result<Json<Vec<PeerTrust>>, Response> {
    api.peer_store
        .peer_trusts()
        .await
        .map(|records| {
            Json(
                records
                    .into_iter()
                    .map(|record| PeerTrust {
                        id: record.id,
                        fingerprint: record.fingerprint.as_str().to_owned(),
                        revision: record.revision,
                        revoked: record.revoked,
                    })
                    .collect(),
            )
        })
        .map_err(rejection)
}

#[utoipa::path(post,path="/v1/peers/trust",request_body=PeerApproval,responses((status=200,body=PeerTrust)),security(("bearer"=[])))]
async fn peer_approve(
    State(api): State<Arc<Api>>,
    Json(input): Json<PeerApproval>,
) -> Result<Json<PeerTrust>, Response> {
    let certificate = BASE64
        .decode(input.certificate.as_bytes())
        .map_err(rejection)?;
    let fingerprint =
        okytes_core::peer::Hash::try_from(input.confirmed_fingerprint).map_err(rejection)?;
    crate::peer_identity::ApprovedPeer::confirm(&certificate, &fingerprint).map_err(rejection)?;
    api.peer_store
        .peer_approve(input.id, certificate, fingerprint, input.expected_revision)
        .await
        .map(|record| {
            Json(PeerTrust {
                id: record.id,
                fingerprint: record.fingerprint.as_str().to_owned(),
                revision: record.revision,
                revoked: record.revoked,
            })
        })
        .map_err(rejection)
}

#[utoipa::path(delete,path="/v1/peers/trust/{id}/{revision}",params(("id"=uuid::Uuid,Path),("revision"=u64,Path)),responses((status=200,body=PeerTrust)),security(("bearer"=[])))]
async fn peer_revoke(
    State(api): State<Arc<Api>>,
    Path((id, revision)): Path<(String, u64)>,
) -> Result<Json<PeerTrust>, Response> {
    let id = uuid::Uuid::parse_str(&id).map_err(rejection)?;
    api.peer_store
        .peer_revoke(id, revision)
        .await
        .map(|record| {
            Json(PeerTrust {
                id: record.id,
                fingerprint: record.fingerprint.as_str().to_owned(),
                revision: record.revision,
                revoked: record.revoked,
            })
        })
        .map_err(rejection)
}

#[utoipa::path(get,path="/v1/peers/requests",params(("offset"=Option<u32>,Query)),responses((status=200,body=[PeerRequest])),security(("bearer"=[])))]
async fn peer_requests(
    State(api): State<Arc<Api>>,
    Query(page): Query<Page>,
) -> Result<Json<Vec<PeerRequest>>, Response> {
    let records = api
        .peer_store
        .peer_requests(page.offset as usize)
        .await
        .map_err(rejection)?;
    let mut requests = Vec::with_capacity(records.len());
    for record in records {
        requests.push(peer_request_view(&api, record).await?);
    }
    Ok(Json(requests))
}

async fn peer_request_view(
    api: &Api,
    record: okytes_persistence::peer::Request,
) -> Result<PeerRequest, Response> {
    let offsets = api
        .peer_store
        .peer_offsets(record.id, record.peer.clone())
        .await
        .map_err(rejection)?;
    let total = record.manifest.validate().map_err(rejection)?;
    let failure = api
        .peer_store
        .peer_failure(record.id)
        .await
        .map_err(rejection)?;
    let destination = if record.direction == okytes_core::peer::Direction::Inbound {
        api.peer_store
            .peer_inbound_destination(record.id)
            .await
            .map_err(rejection)?
    } else {
        None
    };
    let mut transferred = 0u64;
    let mut files_completed = 0u32;
    for entry in &record.manifest.entries {
        if let okytes_core::peer::Entry::File { path, size, .. } = entry {
            let offset = offsets.get(path).copied().unwrap_or_default().min(*size);
            transferred = transferred.saturating_add(offset);
            files_completed += u32::from(offset == *size);
        }
    }
    Ok(PeerRequest {
        id: record.id,
        peer_fingerprint: record.peer.as_str().to_owned(),
        direction: record.direction,
        manifest: record.manifest,
        state: record.state,
        revision: record.revision,
        bytes: transferred.min(total),
        total,
        files_completed,
        failure,
        destination,
    })
}

#[utoipa::path(post,path="/v1/peers/send",request_body=PeerSend,responses((status=200,body=String)),security(("bearer"=[])))]
async fn peer_send(
    State(api): State<Arc<Api>>,
    Json(input): Json<PeerSend>,
) -> Result<Json<String>, Response> {
    if input.paths.is_empty() || input.paths.len() > 32 {
        return Err(rejection("invalid peer source selection"));
    }
    let address: std::net::SocketAddr = input.address.parse().map_err(rejection)?;
    let paths = input
        .paths
        .into_iter()
        .map(std::path::PathBuf::from)
        .collect::<Vec<_>>();
    if paths.iter().any(|path| !path.is_absolute()) {
        return Err(rejection("peer source paths must be absolute"));
    }
    let trust = api
        .peer_store
        .peer_trust(input.peer_id)
        .await
        .map_err(rejection)?
        .filter(|record| !record.revoked)
        .ok_or_else(|| rejection("peer is not trusted"))?;
    let source_paths = paths.clone();
    let source =
        tokio::task::spawn_blocking(move || crate::peer_transfer::Source::open(&source_paths))
            .await
            .map_err(rejection)?
            .map_err(rejection)?;
    let request_id = uuid::Uuid::new_v4();
    let created = api
        .peer_store
        .peer_offer(
            request_id,
            trust.fingerprint.clone(),
            okytes_core::peer::Direction::Outbound,
            source.manifest().clone(),
        )
        .await
        .map_err(rejection)?;
    api.peer_store
        .peer_transition(
            request_id,
            trust.fingerprint.clone(),
            created.revision,
            okytes_core::peer::State::AwaitingApproval,
        )
        .await
        .map_err(rejection)?;
    let outbound = okytes_persistence::peer::Outbound {
        request_id,
        peer_id: input.peer_id,
        address: address.to_string(),
        paths: paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
    };
    api.peer_store
        .peer_save_outbound(outbound.clone())
        .await
        .map_err(rejection)?;
    launch_outbound(api.clone(), outbound).await?;
    Ok(Json(request_id.to_string()))
}

async fn launch_outbound(
    api: Arc<Api>,
    outbound: okytes_persistence::peer::Outbound,
) -> Result<(), Response> {
    let address: std::net::SocketAddr = outbound.address.parse().map_err(rejection)?;
    let paths = outbound
        .paths
        .iter()
        .map(std::path::PathBuf::from)
        .collect::<Vec<_>>();
    let source = Arc::new(
        tokio::task::spawn_blocking(move || crate::peer_transfer::Source::open(&paths))
            .await
            .map_err(rejection)?
            .map_err(rejection)?,
    );
    let request = api
        .peer_store
        .peer_request(outbound.request_id)
        .await
        .map_err(rejection)?
        .ok_or_else(|| rejection("peer request not found"))?;
    if source.manifest() != &request.manifest {
        let _ = api
            .peer_store
            .peer_set_failure(
                outbound.request_id,
                "The selected source changed after the transfer was created.".to_owned(),
            )
            .await;
        let _ = api
            .peer_store
            .peer_transition(
                request.id,
                request.peer.clone(),
                request.revision,
                okytes_core::peer::State::Failed,
            )
            .await;
        return Err(rejection("outbound source changed since it was offered"));
    }
    let request_id = outbound.request_id;
    let peer_id = outbound.peer_id;
    let fingerprint = request.peer;
    let task_api = api.clone();
    let task = tokio::spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(15 * 60);
        loop {
            let current = match task_api.peer_store.peer_request(request_id).await {
                Ok(Some(record)) => record,
                _ => break,
            };
            if matches!(
                current.state,
                okytes_core::peer::State::Cancelled
                    | okytes_core::peer::State::Paused
                    | okytes_core::peer::State::Rejected
                    | okytes_core::peer::State::Completed
            ) {
                break;
            }
            let tls = match task_api
                .peer_identity
                .client_for(&task_api.peer_store, peer_id)
                .await
            {
                Ok(config) => config,
                Err(_) => break,
            };
            match crate::peer_session::send_managed(
                address,
                tls,
                &source,
                request_id,
                &task_api.peer_store,
                fingerprint.clone(),
            )
            .await
            {
                Ok(()) => {
                    let _ = task_api.peer_store.peer_remove_outbound(request_id).await;
                    break;
                }
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_secs(2)).await
                }
                Err(error) => {
                    let _ = task_api
                        .peer_store
                        .peer_set_failure(
                            request_id,
                            format!("Peer connection stopped: {}", error.kind()),
                        )
                        .await;
                    if let Ok(Some(current)) = task_api.peer_store.peer_request(request_id).await {
                        if matches!(
                            current.state,
                            okytes_core::peer::State::Negotiating
                                | okytes_core::peer::State::Transferring
                                | okytes_core::peer::State::Retrying
                        ) {
                            let _ = task_api
                                .peer_store
                                .peer_transition(
                                    request_id,
                                    current.peer,
                                    current.revision,
                                    okytes_core::peer::State::Interrupted,
                                )
                                .await;
                        }
                    }
                    tracing::warn!(%request_id,%error,"outbound peer transfer stopped");
                    break;
                }
            }
        }
    });
    let mut tasks = api.peer_tasks.lock().await;
    tasks.retain(|task| !task.is_finished());
    if tasks.len() >= 64 {
        task.abort();
        return Err(error(StatusCode::TOO_MANY_REQUESTS, "busy"));
    }
    tasks.push(task);
    Ok(())
}

#[utoipa::path(get,path="/v1/peers/discovery",responses((status=200,body=[PeerDevice])),security(("bearer"=[])))]
async fn peer_devices(State(api): State<Arc<Api>>) -> Result<Json<Vec<PeerDevice>>, Response> {
    let discovery = api.discovery.read().await.clone();
    let Some(discovery) = discovery else {
        return Ok(Json(Vec::new()));
    };
    let trusted = api
        .peer_store
        .peer_active_trusts()
        .await
        .map_err(rejection)?
        .into_iter()
        .map(|record| (record.fingerprint.as_str().to_owned(), record.id))
        .collect::<std::collections::HashMap<_, _>>();
    Ok(Json(
        discovery
            .devices()
            .await
            .into_iter()
            .map(|device| PeerDevice {
                peer_id: trusted.get(&device.fingerprint).copied(),
                trusted: trusted.contains_key(&device.fingerprint),
                fullname: device.fullname,
                name: device.name,
                os: device.os,
                protocol: device.protocol,
                fingerprint: device.fingerprint,
                addresses: device.addresses,
                port: device.port,
            })
            .collect(),
    ))
}

#[utoipa::path(get,path="/v1/peers/discovery/settings",responses((status=200,body=PeerDiscoverySetting)),security(("bearer"=[])))]
async fn peer_discovery_setting(State(api): State<Arc<Api>>) -> Json<PeerDiscoverySetting> {
    Json(PeerDiscoverySetting {
        enabled: api.discovery.read().await.is_some(),
    })
}

#[utoipa::path(post,path="/v1/peers/discovery/settings",request_body=PeerDiscoverySetting,responses((status=200,body=PeerDiscoverySetting)),security(("bearer"=[])))]
async fn peer_set_discovery(
    State(api): State<Arc<Api>>,
    Json(input): Json<PeerDiscoverySetting>,
) -> Result<Json<PeerDiscoverySetting>, Response> {
    api.set_peer_discovery(input.enabled)
        .await
        .map_err(rejection)?;
    Ok(Json(input))
}

#[utoipa::path(post,path="/v1/peers/requests/{id}/decision",params(("id"=uuid::Uuid,Path)),request_body=PeerDecision,responses((status=200,body=PeerRequest)),security(("bearer"=[])))]
async fn peer_decide(
    State(api): State<Arc<Api>>,
    Path(id): Path<String>,
    Json(input): Json<PeerDecision>,
) -> Result<Json<PeerRequest>, Response> {
    let id = uuid::Uuid::parse_str(&id).map_err(rejection)?;
    let current = api
        .peer_store
        .peer_request(id)
        .await
        .map_err(rejection)?
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "not_found"))?;
    if current.revision != input.expected_revision {
        return Err(error(StatusCode::CONFLICT, "revision_conflict"));
    }
    if matches!(&input.action, PeerAction::Accept)
        && current.direction == okytes_core::peer::Direction::Inbound
    {
        let selected = match input.destination.as_deref() {
            Some(value) => std::path::PathBuf::from(value),
            None => std::path::PathBuf::from(
                api.session.settings().await.map_err(rejection)?.destination,
            ),
        };
        if !selected.is_absolute() {
            return Err(rejection("peer destination must be absolute"));
        }
        tokio::fs::create_dir_all(&selected)
            .await
            .map_err(rejection)?;
        api.peer_store
            .peer_set_inbound_destination(id, selected.to_string_lossy().into_owned())
            .await
            .map_err(rejection)?;
    }
    let retrying = matches!(&input.action, PeerAction::Retry);
    let record = match input.action {
        PeerAction::Accept => {
            api.peer_store
                .peer_transition(
                    id,
                    current.peer.clone(),
                    current.revision,
                    okytes_core::peer::State::Negotiating,
                )
                .await
        }
        PeerAction::Reject => {
            api.peer_store
                .peer_transition(
                    id,
                    current.peer.clone(),
                    current.revision,
                    okytes_core::peer::State::Rejected,
                )
                .await
        }
        PeerAction::Pause => {
            api.peer_store
                .peer_transition(
                    id,
                    current.peer.clone(),
                    current.revision,
                    okytes_core::peer::State::Paused,
                )
                .await
        }
        PeerAction::Cancel => {
            api.peer_store
                .peer_transition(
                    id,
                    current.peer.clone(),
                    current.revision,
                    okytes_core::peer::State::Cancelled,
                )
                .await
        }
        PeerAction::Retry => {
            let retry = api
                .peer_store
                .peer_transition(
                    id,
                    current.peer.clone(),
                    current.revision,
                    okytes_core::peer::State::Retrying,
                )
                .await
                .map_err(rejection)?;
            api.peer_store
                .peer_transition(
                    id,
                    current.peer.clone(),
                    retry.revision,
                    okytes_core::peer::State::Negotiating,
                )
                .await
        }
    }
    .map_err(rejection)?;
    if retrying && record.direction == okytes_core::peer::Direction::Outbound {
        let outbound = api
            .peer_store
            .peer_outbounds()
            .await
            .map_err(rejection)?
            .into_iter()
            .find(|outbound| outbound.request_id == id)
            .ok_or_else(|| rejection("outbound recovery record not found"))?;
        launch_outbound(api.clone(), outbound).await?;
    }
    peer_request_view(&api, record).await.map(Json)
}
#[utoipa::path(post,path="/v1/quick/pair",responses((status=200,body=crate::quick::Pairing)),security(("bearer"=[])))]
async fn quick_pair(State(api): State<Arc<Api>>) -> Result<Json<crate::quick::Pairing>, Response> {
    api.quick.pair().await.map(Json)
}
#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    offset: u32,
}

fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, Response> {
    let Some(value) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_idempotency_key"))?;
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_idempotency_key"));
    }
    Ok(Some(value.to_owned()))
}

fn request_digest(value: &impl Serialize) -> Result<String, Response> {
    let bytes = serde_json::to_vec(value).map_err(rejection)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn idempotency_rejection(cause: impl std::fmt::Display) -> Response {
    if cause
        .to_string()
        .contains("idempotency request is still pending")
    {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                error: "idempotency_pending".into(),
                category: ErrorCategory::Unavailable,
                retryable: true,
                message: "The original request may still be committing; retry the same key later."
                    .into(),
            }),
        )
            .into_response()
    } else if cause.to_string().contains("idempotency key conflicts") {
        (
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "idempotency_conflict".into(),
                category: ErrorCategory::Conflict,
                retryable: false,
                message: "The idempotency key was already used with a different request.".into(),
            }),
        )
            .into_response()
    } else {
        rejection(cause)
    }
}
#[utoipa::path(get,path="/v1/jobs",params(("offset"=Option<u32>,Query)),responses((status=200,body=Snapshot)),security(("bearer"=[])))]
async fn list(
    State(api): State<Arc<Api>>,
    Query(page): Query<Page>,
    browser: Option<axum::Extension<crate::quick::BrowserRequest>>,
) -> Result<Json<Snapshot>, Response> {
    let mut snapshot = api.session.list(page.offset).await.map_err(rejection)?;
    if browser.is_some() {
        for job in &mut snapshot.jobs {
            job.url.clear();
            job.error = None;
            job.etag = None;
            job.segments.clear();
        }
    }
    Ok(Json(snapshot))
}
#[utoipa::path(post,path="/v1/jobs",request_body=AddInput,responses((status=200,body=Vec<String>)),security(("bearer"=[])))]
async fn add(
    State(api): State<Arc<Api>>,
    axum::Extension(info): axum::Extension<TokenInfo>,
    headers: HeaderMap,
    Json(input): Json<AddInput>,
) -> Result<Json<Vec<String>>, Response> {
    destination(&api, info.scope, input.destination.as_deref()).await?;
    let key = idempotency_key(&headers)?;
    let digest = request_digest(&input)?;
    let _serial = api.idempotency.lock().await;
    if let Some(key) = key.as_deref() {
        if let Some(response) = api
            .peer_store
            .idempotency_result(&info.id, "jobs", key, &digest)
            .await
            .map_err(idempotency_rejection)?
        {
            return serde_json::from_str(&response).map(Json).map_err(rejection);
        }
        api.peer_store
            .reserve_idempotency(&info.id, "jobs", key, &digest)
            .await
            .map_err(idempotency_rejection)?;
    }
    let result = match api.session.add(input).await {
        Ok(result) => result,
        Err(cause) => {
            if let Some(key) = key.as_deref() {
                let _ = api
                    .peer_store
                    .release_idempotency(&info.id, "jobs", key, &digest)
                    .await;
            }
            return Err(if cause.to_string().contains("duplicate") {
                error(StatusCode::CONFLICT, "duplicate_url")
            } else {
                rejection(cause)
            });
        }
    };
    if let Some(key) = key.as_deref() {
        let response = serde_json::to_string(&result).map_err(rejection)?;
        api.peer_store
            .store_idempotency_result(&info.id, "jobs", key, &digest, &response)
            .await
            .map_err(rejection)?;
    }
    Ok(Json(result))
}
#[utoipa::path(post,path="/v1/submit",request_body=Submit,responses((status=200,body=String)),security(("bearer"=[])))]
async fn submit(
    State(api): State<Arc<Api>>,
    axum::Extension(info): axum::Extension<TokenInfo>,
    headers: HeaderMap,
    Json(input): Json<Submit>,
) -> Result<Json<String>, Response> {
    destination(&api, info.scope, input.destination.as_deref()).await?;
    let key = idempotency_key(&headers)?;
    let digest = request_digest(&input)?;
    let _serial = api.idempotency.lock().await;
    if let Some(key) = key.as_deref() {
        if let Some(response) = api
            .peer_store
            .idempotency_result(&info.id, "submit", key, &digest)
            .await
            .map_err(idempotency_rejection)?
        {
            return serde_json::from_str(&response).map(Json).map_err(rejection);
        }
        api.peer_store
            .reserve_idempotency(&info.id, "submit", key, &digest)
            .await
            .map_err(idempotency_rejection)?;
    }
    let result = match api.session.submit(input).await {
        Ok(result) => result,
        Err(cause) => {
            if let Some(key) = key.as_deref() {
                let _ = api
                    .peer_store
                    .release_idempotency(&info.id, "submit", key, &digest)
                    .await;
            }
            return Err(rejection(cause));
        }
    };
    if let Some(key) = key.as_deref() {
        let response = serde_json::to_string(&result).map_err(rejection)?;
        api.peer_store
            .store_idempotency_result(&info.id, "submit", key, &digest, &response)
            .await
            .map_err(rejection)?;
    }
    Ok(Json(result))
}

#[utoipa::path(get,path="/v1/records",params(("offset"=Option<u32>,Query)),responses((status=200,body=Vec<okytes_core::types::JobRecord>)),security(("bearer"=[])))]
async fn records(
    State(api): State<Arc<Api>>,
    Query(page): Query<Page>,
) -> Result<Json<Vec<okytes_core::types::JobRecord>>, Response> {
    if page.offset > 1_000_000 {
        return Err(rejection("invalid offset"));
    }
    api.session
        .engine_page(page.offset as usize)
        .await
        .map(Json)
        .map_err(rejection)
}
#[utoipa::path(get,path="/v1/records/{id}",params(("id"=String,Path)),responses((status=200,body=okytes_core::types::JobRecord)),security(("bearer"=[])))]
async fn record(
    State(api): State<Arc<Api>>,
    Path(id): Path<String>,
) -> Result<Json<okytes_core::types::JobRecord>, Response> {
    let id = uuid::Uuid::parse_str(&id).map_err(rejection)?;
    api.session
        .engine_record(id)
        .await
        .map(Json)
        .map_err(rejection)
}
#[utoipa::path(get,path="/v1/config",responses((status=200,body=okytes_core::types::EngineConfig)),security(("bearer"=[])))]
async fn config(
    State(api): State<Arc<Api>>,
) -> Result<Json<okytes_core::types::EngineConfig>, Response> {
    api.session
        .configuration()
        .await
        .map(Json)
        .map_err(rejection)
}
#[utoipa::path(put,path="/v1/config",request_body=okytes_core::types::EngineConfig,responses((status=200,body=())),security(("bearer"=[])))]
async fn set_config(
    State(api): State<Arc<Api>>,
    Json(config): Json<okytes_core::types::EngineConfig>,
) -> Result<Json<()>, Response> {
    api.session
        .set_configuration(config)
        .await
        .map(Json)
        .map_err(rejection)
}
#[utoipa::path(get,path="/v1/jobs/{id}",params(("id"=String,Path)),responses((status=200,body=Job)),security(("bearer"=[])))]
async fn job(
    State(api): State<Arc<Api>>,
    Path(id): Path<String>,
    browser: Option<axum::Extension<crate::quick::BrowserRequest>>,
) -> Result<Json<Job>, Response> {
    let mut job = api.session.get(id).await.map_err(rejection)?;
    if browser.is_some() {
        job.url.clear();
        job.error = None;
        job.etag = None;
        job.segments.clear();
    }
    Ok(Json(job))
}
#[utoipa::path(post,path="/v1/jobs/{id}/control",params(("id"=String,Path)),request_body=Action,responses((status=200,body=())),security(("bearer"=[])))]
async fn control(
    State(api): State<Arc<Api>>,
    Path(id): Path<String>,
    Json(input): Json<Action>,
) -> Result<Json<()>, Response> {
    api.session
        .control(id, input)
        .await
        .map(Json)
        .map_err(rejection)
}
#[utoipa::path(post,path="/v1/pause-all",responses((status=200,body=())),security(("bearer"=[])))]
async fn pause_all(State(api): State<Arc<Api>>) -> Result<Json<()>, Response> {
    api.session.pause_all().await.map(Json).map_err(rejection)
}
#[utoipa::path(get,path="/v1/settings",responses((status=200,body=Settings)),security(("bearer"=[])))]
async fn settings(State(api): State<Arc<Api>>) -> Result<Json<Settings>, Response> {
    api.session.settings().await.map(Json).map_err(rejection)
}
#[utoipa::path(put,path="/v1/settings",request_body=Settings,responses((status=200,body=())),security(("bearer"=[])))]
async fn set_settings(
    State(api): State<Arc<Api>>,
    Json(input): Json<Settings>,
) -> Result<Json<()>, Response> {
    api.session
        .set_settings(input)
        .await
        .map(Json)
        .map_err(rejection)
}
#[utoipa::path(post,path="/v1/capture",request_body=okytes_browser_bridge::protocol::Request,responses((status=200,body=okytes_browser_bridge::protocol::Response)),security(("bearer"=[])))]
async fn capture(
    State(api): State<Arc<Api>>,
    Json(input): Json<okytes_browser_bridge::protocol::Request>,
) -> Result<Json<okytes_browser_bridge::protocol::Response>, Response> {
    use okytes_browser_bridge::Handler;
    input.validate().map_err(rejection)?;
    Ok(Json(api.session.handle(input).await))
}
#[utoipa::path(get,path="/v1/tokens",responses((status=200,body=Vec<TokenInfo>)),security(("bearer"=[])))]
async fn tokens(State(api): State<Arc<Api>>) -> Json<Vec<TokenInfo>> {
    Json(api.tokens.list().await)
}
#[utoipa::path(post,path="/v1/tokens",request_body=IssueToken,responses((status=200,body=IssuedToken)),security(("bearer"=[])))]
async fn issue(
    State(api): State<Arc<Api>>,
    Json(input): Json<IssueToken>,
) -> Result<Json<IssuedToken>, Response> {
    api.tokens.issue(input).await.map(Json).map_err(rejection)
}
#[utoipa::path(delete,path="/v1/tokens/{id}",params(("id"=String,Path)),responses((status=200,body=bool)),security(("bearer"=[])))]
async fn revoke(
    State(api): State<Arc<Api>>,
    Path(id): Path<String>,
) -> Result<Json<bool>, Response> {
    api.tokens.revoke(&id).await.map(Json).map_err(rejection)
}
#[utoipa::path(get,path="/v1/events",responses((status=200,description="text/event-stream of Update; Resync requires snapshot replacement")),security(("bearer"=[])))]
async fn events(
    State(api): State<Arc<Api>>,
    axum::Extension(info): axum::Extension<TokenInfo>,
    browser: Option<axum::Extension<crate::quick::BrowserRequest>>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Response> {
    let permit = api
        .subscribers
        .clone()
        .try_acquire_owned()
        .map_err(|_| error(StatusCode::TOO_MANY_REQUESTS, "subscriber_limit"))?;
    let receiver = api.session.subscribe().await.map_err(rejection)?;
    let browser = browser.is_some();
    let cursor_api = api.clone();
    let stream = updates(receiver, permit, api, info.id, browser).filter_map(move |update| {
        let cursor_api = cursor_api.clone();
        async move {
            let update = if browser { Update::Resync } else { update };
            serde_json::to_string(&update).ok().map(|value| {
                let cursor = cursor_api
                    .event_cursor
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(Event::default().id(cursor.to_string()).data(value))
            })
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10))))
}
fn updates(
    receiver: tokio::sync::broadcast::Receiver<okytes_core::service::EngineEvent>,
    permit: tokio::sync::OwnedSemaphorePermit,
    api: Arc<Api>,
    id: String,
    browser: bool,
) -> impl Stream<Item = Update> {
    stream::unfold(
        (receiver, permit, true, api, id),
        move |(mut receiver, permit, initial, api, id)| async move {
            let update = if initial {
                Update::Resync
            } else {
                loop {
                    if api.stopping.load(std::sync::atomic::Ordering::Acquire)
                        || !api.tokens.active(&id).await
                        || (browser && !api.quick.active(&id).await)
                    {
                        return None;
                    }
                    tokio::select! {
                        result=receiver.recv()=>break match result {Ok(event)=>Update::Changed(event),Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>Update::Resync,Err(_)=>return None},
                        _=tokio::time::sleep(Duration::from_secs(1))=>{},
                    }
                }
            };
            Some((update, (receiver, permit, false, api, id)))
        },
    )
}
#[utoipa::path(get,path="/v1/openapi.json",responses((status=200,description="Generated OpenAPI 3.1 contract")),security(("bearer"=[])))]
async fn spec() -> Json<utoipa::openapi::OpenApi> {
    Json(openapi())
}
#[utoipa::path(get,path="/v1/capabilities",responses((status=200,body=Capabilities)),security(("bearer"=[])))]
async fn capabilities() -> Json<Capabilities> {
    Json(Capabilities {
        api_version: "1".into(),
        sdk_compatibility: ">=0.1.0,<0.2.0".into(),
        protocols: vec![
            ProtocolCapability {
                protocol: "http".into(),
                operations: vec!["download".into()],
                resume: true,
                integrity: true,
                approval_required: false,
                credential_kind: None,
            },
            ProtocolCapability {
                protocol: "ftp".into(),
                operations: vec!["download".into(), "authorize".into()],
                resume: true,
                integrity: true,
                approval_required: false,
                credential_kind: Some("username_password".into()),
            },
            ProtocolCapability {
                protocol: "sftp".into(),
                operations: vec!["download".into(), "authorize".into()],
                resume: true,
                integrity: true,
                approval_required: false,
                credential_kind: Some("username_password_confirmed_host_key".into()),
            },
            ProtocolCapability {
                protocol: "torrent".into(),
                operations: vec![
                    "inspect".into(),
                    "download".into(),
                    "seed".into(),
                    "select_files".into(),
                ],
                resume: true,
                integrity: true,
                approval_required: false,
                credential_kind: None,
            },
            ProtocolCapability {
                protocol: "media".into(),
                operations: vec!["inspect".into(), "download".into(), "select_tracks".into()],
                resume: true,
                integrity: true,
                approval_required: false,
                credential_kind: None,
            },
            ProtocolCapability {
                protocol: "okytes-peer".into(),
                operations: vec![
                    "discover".into(),
                    "send".into(),
                    "receive".into(),
                    "approve".into(),
                ],
                resume: true,
                integrity: true,
                approval_required: true,
                credential_kind: Some("paired_device_identity".into()),
            },
        ],
        event_delivery: "bounded_sse_snapshot_resync".into(),
        max_page_size: 256,
        max_batch_size: 100,
        idempotency_retention_seconds: 86_400,
        destination_semantics:
            "absolute path on daemon host, restricted by token scope and configured roots".into(),
        credential_policy:
            "request-local credentials are held in memory and never returned by the API".into(),
    })
}
#[derive(OpenApi)]
#[openapi(info(title="Okytes daemon API",version="1.0.0"),paths(quick_pair,inspect_media,authorize_file,list,add,submit,job,control,pause_all,settings,set_settings,capture,tokens,issue,revoke,events,spec,capabilities,records,record,config,set_config,add_torrent,inspect_torrent,torrent,peer_identity,peer_trusts,peer_approve,peer_revoke,peer_requests,peer_send,peer_devices,peer_discovery_setting,peer_set_discovery,peer_decide),components(schemas(Update,ApiError)),modifiers(&Security))]
struct Contract;
struct Security;
impl utoipa::Modify for Security {
    fn modify(&self, spec: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = spec.components.as_mut() {
            components.add_security_scheme(
                "bearer",
                utoipa::openapi::security::SecurityScheme::Http(
                    utoipa::openapi::security::Http::new(
                        utoipa::openapi::security::HttpAuthScheme::Bearer,
                    ),
                ),
            );
        }
        for item in spec.paths.paths.values_mut() {
            for operation in [
                &mut item.get,
                &mut item.put,
                &mut item.post,
                &mut item.delete,
            ]
            .into_iter()
            .flatten()
            {
                for (status, description) in [
                    ("400", "Invalid input"),
                    ("401", "Missing, expired or revoked bearer token"),
                    ("403", "Host, Origin or scope rejected"),
                    ("408", "Request deadline"),
                    ("413", "Request body exceeds limit"),
                    ("429", "Rate, request or subscriber admission limit"),
                ] {
                    let response = utoipa::openapi::response::ResponseBuilder::new()
                        .description(description)
                        .content(
                            "application/json",
                            utoipa::openapi::Content::new(Some(
                                utoipa::openapi::Ref::from_schema_name("ApiError"),
                            )),
                        )
                        .build();
                    operation
                        .responses
                        .responses
                        .insert(status.into(), response.into());
                }
            }
        }
    }
}
/// The server and SDK use the same generated schema. Breaking drift is tested.
pub fn openapi() -> utoipa::openapi::OpenApi {
    Contract::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    #[tokio::test]
    async fn lagged_real_engine_stream_requires_resync_without_unbounded_buffers() {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path().join("okytes.db")).unwrap();
        let mut config = store.config().await.unwrap();
        config.channel_capacity = 2;
        store.set_config(&config).await.unwrap();
        let api = Api::open(
            root.path(),
            &root.path().join("downloads"),
            Policy {
                hosts: vec!["127.0.0.1:17841".into()],
                origins: vec![],
            },
        )
        .await
        .unwrap();
        let source = okytes_test_support::server::TestServer::start(Default::default())
            .await
            .unwrap();
        let receiver = api.session.subscribe().await.unwrap();
        let jobs = api
            .session
            .add(AddInput {
                urls: (0..4)
                    .map(|index| format!("http://{}/normal-{index}", source.addr))
                    .collect(),
                destination: None,
                allow_duplicates: true,
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let mut complete = true;
                for id in &jobs {
                    let job = api.session.get(id.clone()).await.unwrap();
                    assert_ne!(job.state, "Failed", "{:?}", job.error);
                    if job.state != "Completed" {
                        complete = false;
                    }
                }
                if complete {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let id = api.tokens.list().await[0].id.clone();
        let permit = api.subscribers.clone().acquire_owned().await.unwrap();
        let stream = updates(receiver, permit, api, id, false);
        futures::pin_mut!(stream);
        // Initial synchronization plus another Resync caused by actual receiver
        // lag. Event serialization is compared directly to the production type.
        assert!(matches!(stream.next().await, Some(Update::Resync)));
        assert!(matches!(stream.next().await, Some(Update::Resync)));
    }
}

#[utoipa::path(post,path="/v1/torrents",request_body=TorrentInput,responses((status=200,body=String)),security(("bearer"=[])))]
async fn add_torrent(
    State(api): State<Arc<Api>>,
    axum::Extension(info): axum::Extension<TokenInfo>,
    headers: HeaderMap,
    Json(input): Json<TorrentInput>,
) -> Result<Json<String>, Response> {
    destination(&api, info.scope, input.destination.as_deref()).await?;
    let key = idempotency_key(&headers)?;
    let digest = request_digest(&input)?;
    let _serial = api.idempotency.lock().await;
    if let Some(key) = key.as_deref() {
        if let Some(response) = api
            .peer_store
            .idempotency_result(&info.id, "torrents", key, &digest)
            .await
            .map_err(idempotency_rejection)?
        {
            return serde_json::from_str(&response).map(Json).map_err(rejection);
        }
        api.peer_store
            .reserve_idempotency(&info.id, "torrents", key, &digest)
            .await
            .map_err(idempotency_rejection)?;
    }
    let result = match api.session.add_torrent(input).await {
        Ok(result) => result,
        Err(cause) => {
            if let Some(key) = key.as_deref() {
                let _ = api
                    .peer_store
                    .release_idempotency(&info.id, "torrents", key, &digest)
                    .await;
            }
            return Err(rejection(cause));
        }
    };
    if let Some(key) = key.as_deref() {
        let response = serde_json::to_string(&result).map_err(rejection)?;
        api.peer_store
            .store_idempotency_result(&info.id, "torrents", key, &digest, &response)
            .await
            .map_err(rejection)?;
    }
    Ok(Json(result))
}
#[utoipa::path(post,path="/v1/torrents/inspect",request_body=TorrentInput,responses((status=200,body=TorrentMetadata)),security(("bearer"=[])))]
async fn inspect_torrent(
    State(api): State<Arc<Api>>,
    Json(input): Json<TorrentInput>,
) -> Result<Json<TorrentMetadata>, Response> {
    api.session
        .inspect_torrent(input)
        .await
        .map(Json)
        .map_err(rejection)
}
#[utoipa::path(get,path="/v1/torrents/{id}",params(("id"=String,Path)),responses((status=200,body=TorrentSnapshot)),security(("bearer"=[])))]
async fn torrent(
    State(api): State<Arc<Api>>,
    Path(id): Path<String>,
) -> Result<Json<TorrentSnapshot>, Response> {
    api.session.torrent(id).await.map(Json).map_err(rejection)
}

#[utoipa::path(post,path="/v1/media/inspect",request_body=String,responses((status=200,body=Vec<okytes_core::desktop::MediaChoice>)),security(("bearer"=[])))]
async fn inspect_media(
    State(api): State<Arc<Api>>,
    Json(url): Json<String>,
) -> Result<Json<Vec<okytes_core::desktop::MediaChoice>>, Response> {
    api.session
        .inspect_media(url)
        .await
        .map(Json)
        .map_err(rejection)
}
#[utoipa::path(post,path="/v1/jobs/{id}/credentials",params(("id"=String,Path)),request_body=okytes_core::desktop::FileCredentials,responses((status=200)),security(("bearer"=[])))]
async fn authorize_file(
    State(api): State<Arc<Api>>,
    Path(id): Path<String>,
    Json(credentials): Json<okytes_core::desktop::FileCredentials>,
) -> Result<Json<()>, Response> {
    api.session
        .authorize_file(id, credentials)
        .await
        .map(Json)
        .map_err(rejection)
}
