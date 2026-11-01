//! Self-hosted Okytes cloud preview: accounts (email, GitHub, Google), paired
//! devices, a pending download queue (phone-to-PC), history sync and analytics.
//! Data stays in one local SQLite file. Run it on localhost, or behind a TLS
//! reverse proxy before exposing it to phones on a network.
mod db;
mod mail;
mod oauth;

use anyhow::Context;
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
    Json, Router,
};
use clap::Parser;
use db::{Db, HistoryEntry, Principal};
use serde::Deserialize;
use serde_json::json;
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc, sync::Mutex};

#[derive(Parser)]
#[command(
    name = "okytes-cloud",
    about = "Self-hosted Okytes accounts, queue and sync"
)]
struct Args {
    /// Listen address. Keep loopback unless a TLS proxy fronts the server.
    #[arg(long, default_value = "127.0.0.1:17900")]
    bind: SocketAddr,
    /// SQLite database file.
    #[arg(long, default_value = "okytes-cloud.db")]
    db: PathBuf,
    /// Public base URL used for OAuth redirects.
    #[arg(
        long,
        env = "OKYTES_CLOUD_PUBLIC_URL",
        default_value = "http://127.0.0.1:17900"
    )]
    public_url: String,
    /// Comma-separated authorized emails or `@domain` entries. When empty, only
    /// the first account (the owner) may be created.
    #[arg(long, env = "OKYTES_CLOUD_ALLOW", default_value = "")]
    allow: String,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Print a one-hour password-reset link for an account (owner recovery
    /// when no SMTP server is configured). Hand it to the user privately.
    ResetLink {
        #[arg(long)]
        email: String,
    },
}

struct App {
    db: Db,
    public_url: String,
    allow: Vec<String>,
    providers: HashMap<&'static str, oauth::Provider>,
    failures: Mutex<HashMap<String, (u32, i64)>>,
    claims: Mutex<(i64, u32)>,
    mailer: Option<mail::Mailer>,
    forgot: Mutex<(i64, u32)>,
}
type Shared = Arc<App>;

struct Failure(StatusCode, String);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}
impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        tracing::warn!(%error, "request failed");
        Failure(StatusCode::INTERNAL_SERVER_ERROR, "Server error".into())
    }
}
fn fail(status: StatusCode, message: &str) -> Failure {
    Failure(status, message.into())
}
type Reply<T> = Result<T, Failure>;

impl App {
    /// Allowlisted email or domain; with no allowlist, only the owner account.
    fn authorized(&self, email: &str, existing: bool) -> anyhow::Result<bool> {
        if self.allow.is_empty() {
            return Ok(existing || self.db.user_count()? == 0);
        }
        let domain = email.rsplit_once('@').map(|(_, d)| format!("@{d}"));
        Ok(self
            .allow
            .iter()
            .any(|entry| *entry == email || Some(entry) == domain.as_ref()))
    }
    fn principal(&self, headers: &HeaderMap) -> Reply<Principal> {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| fail(StatusCode::UNAUTHORIZED, "Sign in required"))?;
        let principal = self
            .db
            .session(token)?
            .ok_or_else(|| fail(StatusCode::UNAUTHORIZED, "Session expired; sign in again"))?;
        // Removing an email from the allowlist revokes access immediately.
        if !self.authorized(&principal.user.email, true)? {
            return Err(fail(
                StatusCode::FORBIDDEN,
                "This account is no longer authorized",
            ));
        }
        Ok(principal)
    }
    fn device(&self, headers: &HeaderMap) -> Reply<(Principal, String)> {
        let principal = self.principal(headers)?;
        let device = principal
            .device
            .clone()
            .ok_or_else(|| fail(StatusCode::FORBIDDEN, "Only paired devices can do this"))?;
        Ok((principal, device))
    }
    fn dashboard(&self, headers: &HeaderMap) -> Reply<Principal> {
        let principal = self.principal(headers)?;
        if principal.device.is_some() {
            return Err(fail(
                StatusCode::FORBIDDEN,
                "Sign in to the dashboard to do this",
            ));
        }
        Ok(principal)
    }
}

fn valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    email.len() <= 254
        && !local.is_empty()
        && domain.contains('.')
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
}
fn valid_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        value.len() <= 8192
            && matches!(url.scheme(), "http" | "https" | "ftp" | "ftps" | "sftp")
            && url.username().is_empty()
            && url.password().is_none()
            && url.host_str().is_some()
    })
}

#[derive(Deserialize)]
struct Credentials {
    email: String,
    password: String,
}

async fn config(State(app): State<Shared>) -> Reply<Json<serde_json::Value>> {
    Ok(Json(json!({
        "github": app.providers.contains_key("github"),
        "google": app.providers.contains_key("google"),
        "owner_setup": app.allow.is_empty() && app.db.user_count()? == 0,
        "email": app.mailer.is_some(),
    })))
}

async fn register(
    State(app): State<Shared>,
    Json(input): Json<Credentials>,
) -> Reply<Json<serde_json::Value>> {
    let email = input.email.trim().to_lowercase();
    if !valid_email(&email) {
        return Err(fail(StatusCode::BAD_REQUEST, "Enter a valid email address"));
    }
    if !(10..=256).contains(&input.password.chars().count()) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Use a password of at least 10 characters",
        ));
    }
    if !app.authorized(&email, false)? {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "This email is not authorized for this server",
        ));
    }
    if app.db.user_by_email(&email)?.is_some() {
        return Err(fail(
            StatusCode::CONFLICT,
            "An account with this email exists; sign in instead",
        ));
    }
    let hash = tokio::task::spawn_blocking(move || hash_password(&input.password))
        .await
        .context("hash task")??;
    // With email configured, the address must be proven before first sign-in.
    let Some(mailer) = &app.mailer else {
        let user = app.db.create_user(&email, Some(&hash))?;
        let token = app.db.create_session(&user.id, None, 7)?;
        return Ok(Json(json!({ "token": token, "user": user })));
    };
    let user = app.db.create_user_verified(&email, Some(&hash), false)?;
    let token = app.db.email_token(&user.id, "verify")?;
    mailer
        .send(
            &email,
            "Confirm your Okytes Cloud account",
            format!(
                "Open this link within one hour to confirm your email:

{}/v1/auth/verify?token={token}

If you did not create an account, ignore this message.",
                app.public_url
            ),
        )
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "verification email failed");
            fail(StatusCode::BAD_GATEWAY, "The confirmation email could not be sent; ask the server owner to check SMTP settings")
        })?;
    Ok(Json(json!({ "verify": true })))
}

#[derive(Deserialize)]
struct TokenQuery {
    token: String,
}
async fn verify(State(app): State<Shared>, Query(query): Query<TokenQuery>) -> Redirect {
    match app.db.take_email_token(&query.token, "verify") {
        Ok(Some(user)) if app.db.mark_verified(&user).is_ok() => {
            Redirect::to("/#notice=Email%20confirmed.%20You%20can%20sign%20in%20now.")
        }
        _ => Redirect::to("/#error=This%20confirmation%20link%20is%20invalid%20or%20expired."),
    }
}

#[derive(Deserialize)]
struct Forgot {
    email: String,
}
// Always answers the same way so the endpoint cannot be used to probe accounts.
async fn forgot(State(app): State<Shared>, Json(input): Json<Forgot>) -> Reply<StatusCode> {
    {
        let mut window = app.forgot.lock().map_err(|_| anyhow::anyhow!("lock"))?;
        let now = db::now();
        if now - window.0 >= 60 {
            *window = (now, 0);
        }
        window.1 += 1;
        if window.1 > 10 {
            return Err(fail(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many requests; wait a minute",
            ));
        }
    }
    let Some(mailer) = &app.mailer else {
        return Err(fail(
            StatusCode::NOT_IMPLEMENTED,
            "Email is not configured on this server; ask its owner for a reset link",
        ));
    };
    let email = input.email.trim().to_lowercase();
    if let Some((user, _)) = app.db.user_by_email(&email)? {
        if app.authorized(&email, true)? {
            let token = app.db.email_token(&user.id, "reset")?;
            let body = format!(
                "Open this link within one hour to choose a new password:

{}/#reset={token}

If you did not ask for this, ignore this message; your password is unchanged.",
                app.public_url
            );
            if let Err(error) = mailer
                .send(&email, "Reset your Okytes Cloud password", body)
                .await
            {
                tracing::warn!(%error, "reset email failed");
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Reset {
    token: String,
    password: String,
}
async fn reset(State(app): State<Shared>, Json(input): Json<Reset>) -> Reply<StatusCode> {
    if !(10..=256).contains(&input.password.chars().count()) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Use a password of at least 10 characters",
        ));
    }
    let user = app
        .db
        .take_email_token(&input.token, "reset")?
        .ok_or_else(|| {
            fail(
                StatusCode::BAD_REQUEST,
                "This reset link is invalid or expired; request a new one",
            )
        })?;
    let hash = tokio::task::spawn_blocking(move || hash_password(&input.password))
        .await
        .context("hash task")??;
    app.db.reset_password(&user, &hash)?;
    Ok(StatusCode::NO_CONTENT)
}

fn hash_password(password: &str) -> anyhow::Result<String> {
    use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
    let salt = SaltString::generate(&mut OsRng);
    argon2::Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("password hashing failed: {e}"))
}

async fn login(
    State(app): State<Shared>,
    Json(input): Json<Credentials>,
) -> Reply<Json<serde_json::Value>> {
    let email = input.email.trim().to_lowercase();
    let now = db::now();
    {
        let failures = app.failures.lock().map_err(|_| anyhow::anyhow!("lock"))?;
        if failures
            .get(&email)
            .is_some_and(|(n, at)| *n >= 10 && now - at < 900)
        {
            return Err(fail(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many attempts; try again in 15 minutes",
            ));
        }
    }
    let found = app.db.user_by_email(&email)?;
    let password = input.password;
    let stored = found.as_ref().and_then(|(_, hash)| hash.clone());
    // Verify even for unknown accounts so timing does not reveal which exist.
    let ok = tokio::task::spawn_blocking(move || {
        use argon2::password_hash::{PasswordHash, PasswordVerifier};
        let fallback = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$Zm9vYmFyZm9vYmFyZm9vYmFyZm9vYmFyZm9vYmFy";
        let hash = stored.as_deref().unwrap_or(fallback);
        PasswordHash::new(hash).is_ok_and(|parsed| {
            argon2::Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        }) && stored.is_some()
    })
    .await
    .context("verify task")?;
    let user = match (ok, found) {
        (true, Some((user, _))) if app.authorized(&email, true)? => user,
        _ => {
            let mut failures = app.failures.lock().map_err(|_| anyhow::anyhow!("lock"))?;
            if failures.len() > 10_000 {
                failures.clear();
            }
            let entry = failures.entry(email).or_insert((0, now));
            *entry = (entry.0 + 1, now);
            return Err(fail(
                StatusCode::UNAUTHORIZED,
                "Email or password is incorrect",
            ));
        }
    };
    app.failures
        .lock()
        .map_err(|_| anyhow::anyhow!("lock"))?
        .remove(&email);
    if !app.db.is_verified(&user.id)? {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "Confirm your email first using the link we sent you",
        ));
    }
    let token = app.db.create_session(&user.id, None, 7)?;
    Ok(Json(json!({ "token": token, "user": user })))
}

async fn logout(State(app): State<Shared>, headers: HeaderMap) -> Reply<StatusCode> {
    app.principal(&headers)?;
    if let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        app.db.delete_session(token)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn me(State(app): State<Shared>, headers: HeaderMap) -> Reply<Json<serde_json::Value>> {
    let principal = app.principal(&headers)?;
    Ok(Json(
        json!({ "user": principal.user, "device": principal.device }),
    ))
}

async fn oauth_start(State(app): State<Shared>, Path(provider): Path<String>) -> Reply<Redirect> {
    let (name, config) = app
        .providers
        .get_key_value(provider.as_str())
        .ok_or_else(|| {
            fail(
                StatusCode::NOT_FOUND,
                "This sign-in method is not configured",
            )
        })?;
    let state = db::random_hex(16);
    let verifier = db::random_hex(32);
    app.db.save_state(&state, name, &verifier)?;
    let redirect = format!("{}/v1/auth/{name}/callback", app.public_url);
    Ok(Redirect::to(&oauth::authorize_url(
        name, config, &redirect, &state, &verifier,
    )))
}

#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}
// The session token travels in the URL fragment, which browsers never send
// to servers or put in Referer headers; the dashboard moves it to session storage.
async fn oauth_callback(
    State(app): State<Shared>,
    Path(provider): Path<String>,
    Query(query): Query<Callback>,
) -> Redirect {
    let outcome: anyhow::Result<String> = async {
        let config = app
            .providers
            .get(provider.as_str())
            .context("provider not configured")?;
        if let Some(error) = query.error {
            anyhow::bail!("sign-in was cancelled ({error})");
        }
        let (code, state) = query
            .code
            .zip(query.state)
            .context("missing code or state")?;
        let verifier = app
            .db
            .take_state(&state, &provider)?
            .context("sign-in link expired; start again")?;
        let redirect = format!("{}/v1/auth/{provider}/callback", app.public_url);
        let identity = oauth::identity(&provider, config, &code, &redirect, &verifier).await?;
        let existing = app.db.user_by_email(&identity.email)?.is_some();
        if !app.authorized(&identity.email, existing)? {
            anyhow::bail!("{} is not authorized for this server", identity.email);
        }
        let user = app
            .db
            .oauth_user(&provider, &identity.subject, &identity.email)?;
        app.db.create_session(&user.id, None, 7)
    }
    .await;
    match outcome {
        Ok(token) => Redirect::to(&format!("/#token={token}")),
        Err(error) => {
            tracing::info!(%error, "OAuth sign-in refused");
            let message: String =
                url::form_urlencoded::byte_serialize(error.to_string().as_bytes()).collect();
            Redirect::to(&format!("/#error={message}"))
        }
    }
}

async fn pair_code(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Reply<Json<serde_json::Value>> {
    let principal = app.dashboard(&headers)?;
    let code = app.db.create_pair_code(&principal.user.id)?;
    Ok(Json(json!({ "code": code, "expires_in": 600 })))
}

#[derive(Deserialize)]
struct Claim {
    code: String,
    name: String,
    kind: String,
}
async fn pair_claim(
    State(app): State<Shared>,
    Json(input): Json<Claim>,
) -> Reply<Json<serde_json::Value>> {
    {
        // Global limit bounds pairing-code guessing to 30 attempts per minute.
        let mut claims = app.claims.lock().map_err(|_| anyhow::anyhow!("lock"))?;
        let now = db::now();
        if now - claims.0 >= 60 {
            *claims = (now, 0);
        }
        claims.1 += 1;
        if claims.1 > 30 {
            return Err(fail(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many pairing attempts; wait a minute",
            ));
        }
    }
    let name = input.name.trim();
    if name.is_empty()
        || name.len() > 64
        || !matches!(input.kind.as_str(), "desktop" | "phone" | "other")
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Give the device a name up to 64 characters",
        ));
    }
    let (device, token) = app
        .db
        .claim_pair_code(&input.code, name, &input.kind)?
        .ok_or_else(|| fail(StatusCode::UNAUTHORIZED, "Pairing code is wrong or expired"))?;
    Ok(Json(json!({ "device_id": device, "token": token })))
}

async fn devices(State(app): State<Shared>, headers: HeaderMap) -> Reply<Json<serde_json::Value>> {
    let principal = app.principal(&headers)?;
    Ok(Json(
        json!({ "devices": app.db.devices(&principal.user.id)? }),
    ))
}
async fn remove_device(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Reply<StatusCode> {
    let principal = app.dashboard(&headers)?;
    if app.db.delete_device(&principal.user.id, &id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(fail(StatusCode::NOT_FOUND, "Device not found"))
    }
}

#[derive(Deserialize)]
struct Enqueue {
    url: String,
    filename: Option<String>,
    target_device: Option<String>,
}
async fn enqueue(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(input): Json<Enqueue>,
) -> Reply<Json<serde_json::Value>> {
    let principal = app.principal(&headers)?;
    let url = input.url.trim();
    if !valid_url(url) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Use an HTTP(S), FTP or SFTP link without a username or password",
        ));
    }
    let filename = input
        .filename
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty());
    if filename.is_some_and(|f| {
        f.len() > 200 || f.contains(['/', '\\']) || f.chars().any(char::is_control)
    }) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "File name must not contain folders",
        ));
    }
    if let Some(target) = &input.target_device {
        if !app.db.owns_device(&principal.user.id, target)? {
            return Err(fail(StatusCode::BAD_REQUEST, "Unknown target device"));
        }
    }
    let id = app
        .db
        .enqueue(
            &principal.user.id,
            input.target_device.as_deref(),
            url,
            filename,
        )
        .map_err(|e| Failure(StatusCode::CONFLICT, e.to_string()))?;
    Ok(Json(json!({ "id": id })))
}
async fn queue(State(app): State<Shared>, headers: HeaderMap) -> Reply<Json<serde_json::Value>> {
    let principal = app.principal(&headers)?;
    Ok(Json(json!({ "items": app.db.queue(&principal.user.id)? })))
}
async fn remove_queue_item(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Reply<StatusCode> {
    let principal = app.principal(&headers)?;
    if app.db.delete_queue_item(&principal.user.id, &id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(fail(
            StatusCode::CONFLICT,
            "Item is being downloaded or does not exist",
        ))
    }
}
async fn claim(State(app): State<Shared>, headers: HeaderMap) -> Reply<Json<serde_json::Value>> {
    let (principal, device) = app.device(&headers)?;
    Ok(Json(
        json!({ "items": app.db.claim(&principal.user.id, &device)? }),
    ))
}
#[derive(Deserialize)]
struct Report {
    ok: bool,
    job_id: Option<String>,
    error: Option<String>,
}
async fn report(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<Report>,
) -> Reply<StatusCode> {
    let (principal, device) = app.device(&headers)?;
    let error = input.error.map(|e| e.chars().take(500).collect::<String>());
    if app.db.report(
        &principal.user.id,
        &device,
        &id,
        input.ok,
        input.job_id.as_deref(),
        error.as_deref(),
    )? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(fail(
            StatusCode::NOT_FOUND,
            "Item is not claimed by this device",
        ))
    }
}

#[derive(Deserialize)]
struct Sync {
    entries: Vec<HistoryEntry>,
}
async fn sync_history(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(input): Json<Sync>,
) -> Reply<StatusCode> {
    let (principal, device) = app.device(&headers)?;
    if input.entries.len() > 500
        || input.entries.iter().any(|e| {
            e.job_id.len() > 64
                || e.name.len() > 1024
                || e.source.len() > 2048
                || e.state.len() > 32
        })
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Send at most 500 bounded entries per request",
        ));
    }
    app.db
        .sync_history(&principal.user.id, &device, &input.entries)?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
struct Page {
    limit: Option<i64>,
}
async fn history(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(page): Query<Page>,
) -> Reply<Json<serde_json::Value>> {
    let principal = app.principal(&headers)?;
    let limit = page.limit.unwrap_or(200).clamp(1, 1000);
    Ok(Json(
        json!({ "entries": app.db.history(&principal.user.id, limit)? }),
    ))
}
async fn analytics(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Reply<Json<serde_json::Value>> {
    let principal = app.principal(&headers)?;
    Ok(Json(app.db.analytics(&principal.user.id)?))
}

fn asset(body: &'static str, kind: &'static str) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    if let Some(Command::ResetLink { email }) = &args.command {
        let db = Db::open(&args.db)?;
        let (user, _) = db
            .user_by_email(&email.trim().to_lowercase())?
            .context("no account uses that email")?;
        let token = db.email_token(&user.id, "reset")?;
        println!("{}/#reset={token}", args.public_url.trim_end_matches('/'));
        eprintln!("The link works once and expires in one hour.");
        return Ok(());
    }
    let mut providers = HashMap::new();
    for (name, prefix) in [
        ("github", "OKYTES_CLOUD_GITHUB"),
        ("google", "OKYTES_CLOUD_GOOGLE"),
    ] {
        if let (Ok(id), Ok(secret)) = (
            std::env::var(format!("{prefix}_CLIENT_ID")),
            std::env::var(format!("{prefix}_CLIENT_SECRET")),
        ) {
            providers.insert(
                name,
                oauth::Provider {
                    client_id: id,
                    client_secret: secret,
                },
            );
        }
    }
    let allow = args
        .allow
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let app = Arc::new(App {
        db: Db::open(&args.db)?,
        public_url: args.public_url.trim_end_matches('/').to_owned(),
        allow,
        providers,
        failures: Mutex::new(HashMap::new()),
        claims: Mutex::new((0, 0)),
        mailer: mail::Mailer::from_environment()?,
        forgot: Mutex::new((0, 0)),
    });
    let pruning = app.clone();
    tokio::spawn(async move {
        loop {
            if let Err(error) = pruning.db.prune() {
                tracing::warn!(%error, "expired session cleanup failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });
    if !args.bind.ip().is_loopback() && !app.public_url.starts_with("https://") {
        tracing::warn!("listening beyond loopback without an https public URL; passwords and tokens cross the network unencrypted unless a TLS proxy is in front");
    }
    let router = Router::new()
        .route(
            "/",
            get(|| async {
                asset(
                    include_str!("../web/index.html"),
                    "text/html; charset=utf-8",
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                asset(
                    include_str!("../web/app.js"),
                    "text/javascript; charset=utf-8",
                )
            }),
        )
        .route(
            "/app.css",
            get(|| async { asset(include_str!("../web/app.css"), "text/css; charset=utf-8") }),
        )
        .route("/v1/config", get(config))
        .route("/v1/auth/register", post(register))
        .route("/v1/auth/login", post(login))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/auth/verify", get(verify))
        .route("/v1/auth/forgot", post(forgot))
        .route("/v1/auth/reset", post(reset))
        .route("/v1/auth/{provider}/start", get(oauth_start))
        .route("/v1/auth/{provider}/callback", get(oauth_callback))
        .route("/v1/me", get(me))
        .route("/v1/devices", get(devices))
        .route("/v1/devices/pair-code", post(pair_code))
        .route("/v1/devices/claim", post(pair_claim))
        .route("/v1/devices/{id}", delete(remove_device))
        .route("/v1/queue", get(queue).post(enqueue))
        .route("/v1/queue/claim", post(claim))
        .route("/v1/queue/{id}", delete(remove_queue_item))
        .route("/v1/queue/{id}/result", post(report))
        .route("/v1/history", get(history).put(sync_history))
        .route("/v1/analytics", get(analytics))
        .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    tracing::info!("Okytes cloud listening on http://{}", args.bind);
    axum::serve(listener, router).await?;
    Ok(())
}
