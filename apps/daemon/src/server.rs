//! Authenticated v1 API; every route, including errors and the contract, passes
//! the same Host/Origin, authentication, scope and resource admission checks.
use crate::auth::Tokens;
use axum::{
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{Method, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures::{stream, Stream, StreamExt};
use okytes_core::desktop::DesktopService;
use okytes_daemon_api::contract::*;
use okytes_desktop_service::Session;
use okytes_persistence::SqliteStore;
use okytes_torrent_types::{
    Input as TorrentInput, Metadata as TorrentMetadata, Snapshot as TorrentSnapshot, TorrentService,
};
use serde::Deserialize;
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
    stopping: std::sync::atomic::AtomicBool,
    /// Real embedded queue owner used by daemon and desktop.
    pub session: Arc<Session>,
    /// Revocable credential registry.
    pub tokens: Arc<Tokens>,
    policy: Policy,
    rate: Mutex<Rate>,
    requests: Arc<Semaphore>,
    subscribers: Arc<Semaphore>,
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
        let tokens = Tokens::open(
            Arc::new(
                SqliteStore::open(&database)
                    .map_err(|_| io::Error::other("cannot open token store"))?,
            ),
            directory,
        )
        .await?;
        Ok(Arc::new(Self {
            stopping: std::sync::atomic::AtomicBool::new(false),
            session,
            tokens,
            policy,
            rate: Mutex::new(Rate {
                since: Instant::now(),
                total: 0,
                tokens: HashMap::new(),
            }),
            requests: Arc::new(Semaphore::new(32)),
            subscribers: Arc::new(Semaphore::new(16)),
        }))
    }
    /// Reject new requests before draining durable engine writes.
    pub async fn shutdown(&self) -> io::Result<()> {
        self.stopping
            .store(true, std::sync::atomic::Ordering::Release);
        self.session
            .shutdown()
            .await
            .map_err(|_| io::Error::other("queue shutdown failed"))
    }
}
fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(ApiError { error: code.into() })).into_response()
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
        if !origin
            .to_str()
            .ok()
            .is_some_and(|origin| api.policy.origins.iter().any(|allowed| allowed == origin))
        {
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
        .route("/v1/openapi.json", get(spec))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not_found") })
        .layer(DefaultBodyLimit::max(1048576))
        .layer(middleware::from_fn_with_state(api.clone(), guard))
        .with_state(api)
}
#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    offset: u32,
}
#[utoipa::path(get,path="/v1/jobs",params(("offset"=Option<u32>,Query)),responses((status=200,body=Snapshot)),security(("bearer"=[])))]
async fn list(
    State(api): State<Arc<Api>>,
    Query(page): Query<Page>,
) -> Result<Json<Snapshot>, Response> {
    api.session
        .list(page.offset)
        .await
        .map(Json)
        .map_err(rejection)
}
#[utoipa::path(post,path="/v1/jobs",request_body=AddInput,responses((status=200,body=Vec<String>)),security(("bearer"=[])))]
async fn add(
    State(api): State<Arc<Api>>,
    axum::Extension(info): axum::Extension<TokenInfo>,
    Json(input): Json<AddInput>,
) -> Result<Json<Vec<String>>, Response> {
    destination(&api, info.scope, input.destination.as_deref()).await?;
    api.session.add(input).await.map(Json).map_err(rejection)
}
#[utoipa::path(post,path="/v1/submit",request_body=Submit,responses((status=200,body=String)),security(("bearer"=[])))]
async fn submit(
    State(api): State<Arc<Api>>,
    axum::Extension(info): axum::Extension<TokenInfo>,
    Json(input): Json<Submit>,
) -> Result<Json<String>, Response> {
    destination(&api, info.scope, input.destination.as_deref()).await?;
    api.session.submit(input).await.map(Json).map_err(rejection)
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
async fn job(State(api): State<Arc<Api>>, Path(id): Path<String>) -> Result<Json<Job>, Response> {
    api.session.get(id).await.map(Json).map_err(rejection)
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
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Response> {
    let permit = api
        .subscribers
        .clone()
        .try_acquire_owned()
        .map_err(|_| error(StatusCode::TOO_MANY_REQUESTS, "subscriber_limit"))?;
    let receiver = api.session.subscribe().await.map_err(rejection)?;
    let stream = updates(receiver, permit, api, info.id).filter_map(|update| async move {
        serde_json::to_string(&update)
            .ok()
            .map(|value| Ok(Event::default().data(value)))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10))))
}
fn updates(
    receiver: tokio::sync::broadcast::Receiver<okytes_core::service::EngineEvent>,
    permit: tokio::sync::OwnedSemaphorePermit,
    api: Arc<Api>,
    id: String,
) -> impl Stream<Item = Update> {
    stream::unfold(
        (receiver, permit, true, api, id),
        |(mut receiver, permit, initial, api, id)| async move {
            let update = if initial {
                Update::Resync
            } else {
                loop {
                    if api.stopping.load(std::sync::atomic::Ordering::Acquire)
                        || !api.tokens.active(&id).await
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
#[derive(OpenApi)]
#[openapi(info(title="Okytes daemon API",version="1.0.0"),paths(inspect_media,authorize_file,list,add,submit,job,control,pause_all,settings,set_settings,capture,tokens,issue,revoke,events,spec,records,record,config,set_config,add_torrent,inspect_torrent,torrent),components(schemas(Update,ApiError)),modifiers(&Security))]
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
        let stream = updates(receiver, permit, api, id);
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
    Json(input): Json<TorrentInput>,
) -> Result<Json<String>, Response> {
    destination(&api, info.scope, input.destination.as_deref()).await?;
    api.session
        .add_torrent(input)
        .await
        .map(Json)
        .map_err(rejection)
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
