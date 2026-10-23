//! Loopback browser sessions. Pairing grants only the existing control scope;
//! HTTP cookies never replace exact-origin checks or anti-CSRF authorization.
use crate::server::Api;
use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use okytes_daemon_api::contract::{IssueToken, Scope};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io,
    net::SocketAddr,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

include!(concat!(env!("OUT_DIR"), "/quick-assets.rs"));
const LIFETIME: u32 = 900;
const BODY_LIMIT: usize = 98304;

struct Code {
    digest: [u8; 32],
    until: Instant,
}
struct Browser {
    cookie: String,
    csrf: String,
    token: String,
    token_id: String,
    until: Instant,
}
#[derive(Default)]
struct Registry {
    codes: Vec<Code>,
    browsers: Vec<Browser>,
}
/// Explicitly enabled browser surface; secrets are bounded and never Debug.
#[derive(Default)]
pub struct Quick {
    origin: OnceLock<String>,
    cookie_name: OnceLock<String>,
    registry: Mutex<Registry>,
}
/// Native-only issuance reply. The code is displayed once by a trusted client.
pub use okytes_daemon_api::contract::QuickPairing as Pairing;
#[derive(Clone)]
pub(crate) struct BrowserRequest;
fn secret() -> String {
    let bytes: [u8; 32] = rand::random();
    bytes.iter().map(|value| format!("{value:02x}")).collect()
}
fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}
fn equal(left: &str, right: &str) -> bool {
    bool::from(left.as_bytes().ct_eq(right.as_bytes()))
}
fn error(status: StatusCode) -> Response {
    (
        status,
        Json(serde_json::json!({"error":"quick_request_rejected"})),
    )
        .into_response()
}
impl Registry {
    fn prune(&mut self) {
        let now = Instant::now();
        self.codes.retain(|entry| entry.until > now);
        self.browsers.retain(|entry| entry.until > now);
    }
}
impl Quick {
    pub(crate) async fn active(&self, id: &str) -> bool {
        self.registry
            .lock()
            .await
            .browsers
            .iter()
            .any(|entry| entry.token_id == id && entry.until > Instant::now())
    }
    /// Enable only an explicitly configured IP loopback authority, never a LAN host.
    pub fn enable(&self, authority: &str) -> io::Result<()> {
        let address: SocketAddr = authority.parse().map_err(io::Error::other)?;
        if !address.ip().is_loopback() || address.port() == 0 || self.origin.get().is_some() {
            return Err(io::Error::other(
                "Quick mode requires one fixed loopback endpoint",
            ));
        }
        self.cookie_name
            .set(format!("okytes_quick_{}", address.port()))
            .map_err(|_| io::Error::other("Quick mode already configured"))?;
        self.origin
            .set(format!("http://{address}"))
            .map_err(|_| io::Error::other("Quick mode already configured"))
    }
    pub(crate) fn origin_allowed(&self, origin: &str) -> bool {
        self.origin.get().is_some_and(|allowed| origin == allowed)
    }
    fn cookie<'a>(&self, request: &'a axum::http::HeaderMap) -> Option<&'a str> {
        let name = self.cookie_name.get()?;
        request
            .get(header::COOKIE)?
            .to_str()
            .ok()?
            .split(';')
            .filter_map(|part| part.trim().split_once('='))
            .find_map(|(key, value)| (key == name && value.len() == 64).then_some(value))
    }
    fn browser_headers(&self, request: &axum::http::HeaderMap) -> bool {
        self.origin.get().is_some()
            && request
                .get("x-okytes-quick")
                .is_some_and(|value| value == "1")
            && request.get(header::ORIGIN).is_none_or(|value| {
                value
                    .to_str()
                    .is_ok_and(|origin| self.origin_allowed(origin))
            })
            && request
                .get("sec-fetch-site")
                .is_none_or(|value| value == "same-origin")
    }
    pub(crate) async fn authenticate(
        &self,
        request: &axum::http::HeaderMap,
        csrf: bool,
    ) -> Option<(String, String)> {
        if !self.browser_headers(request) {
            return None;
        }
        let cookie = self.cookie(request)?;
        let mut registry = self.registry.lock().await;
        registry.prune();
        let browser = registry
            .browsers
            .iter()
            .find(|entry| equal(&entry.cookie, cookie))?;
        if csrf
            && !request
                .get("x-okytes-csrf")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| equal(value, &browser.csrf))
        {
            return None;
        }
        Some((browser.token.clone(), browser.csrf.clone()))
    }
    pub(crate) async fn pair(&self) -> Result<Pairing, Response> {
        let origin = self
            .origin
            .get()
            .ok_or_else(|| error(StatusCode::NOT_FOUND))?;
        let mut registry = self.registry.lock().await;
        registry.prune();
        if registry.codes.len() >= 8 {
            return Err(error(StatusCode::TOO_MANY_REQUESTS));
        }
        let code = secret();
        registry.codes.push(Code {
            digest: digest(&code),
            until: Instant::now() + Duration::from_secs(120),
        });
        Ok(Pairing {
            url: format!("{origin}/quick"),
            code,
            expires_in: 120,
        })
    }
}

async fn body(request: Request) -> Result<Vec<u8>, Response> {
    tokio::time::timeout(
        Duration::from_secs(8),
        to_bytes(request.into_body(), BODY_LIMIT),
    )
    .await
    .map_err(|_| error(StatusCode::REQUEST_TIMEOUT))?
    .map(|bytes| bytes.to_vec())
    .map_err(|_| error(StatusCode::PAYLOAD_TOO_LARGE))
}

pub(crate) async fn public(api: &Arc<Api>, request: Request) -> Response {
    if api.quick.origin.get().is_none() {
        return error(StatusCode::NOT_FOUND);
    }
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    if path == "/quick/session" {
        if !api.quick.browser_headers(request.headers()) {
            return error(StatusCode::FORBIDDEN);
        }
        if method == Method::GET {
            // Loopback cookies are not port scoped. Never recover the proof
            // from a cookie alone: another local listener may receive cookies.
            // A newly opened tab must obtain explicit native approval again.
            let Some((token, csrf)) = api.quick.authenticate(request.headers(), true).await else {
                return error(StatusCode::UNAUTHORIZED);
            };
            if api.tokens.authenticate(&token).await.is_none() {
                return error(StatusCode::UNAUTHORIZED);
            }
            return Json(serde_json::json!({"csrf":csrf})).into_response();
        }
        if method != Method::POST
            || request
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                != Some("application/json")
        {
            return error(StatusCode::BAD_REQUEST);
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Redeem {
            code: String,
        }
        let Ok(bytes) = body(request).await else {
            return error(StatusCode::BAD_REQUEST);
        };
        let Ok(input) = serde_json::from_slice::<Redeem>(&bytes) else {
            return error(StatusCode::BAD_REQUEST);
        };
        if input.code.len() != 64 {
            return error(StatusCode::UNAUTHORIZED);
        }
        let mut registry = api.quick.registry.lock().await;
        registry.prune();
        if registry.browsers.len() >= 32 {
            return error(StatusCode::TOO_MANY_REQUESTS);
        }
        let submitted = digest(&input.code);
        let Some(index) = registry
            .codes
            .iter()
            .position(|code| bool::from(code.digest.ct_eq(&submitted)))
        else {
            return error(StatusCode::UNAUTHORIZED);
        };
        registry.codes.swap_remove(index);
        let Ok(issued) = api
            .tokens
            .issue(IssueToken {
                scope: Scope::Control,
                lifetime_secs: LIFETIME,
            })
            .await
        else {
            return error(StatusCode::SERVICE_UNAVAILABLE);
        };
        let cookie = secret();
        let csrf = secret();
        registry.browsers.push(Browser {
            cookie: cookie.clone(),
            csrf: csrf.clone(),
            token: issued.token,
            token_id: issued.info.id,
            until: Instant::now() + Duration::from_secs(u64::from(LIFETIME)),
        });
        let Some(name) = api.quick.cookie_name.get() else {
            return error(StatusCode::SERVICE_UNAVAILABLE);
        };
        let mut response = Json(serde_json::json!({"csrf":csrf})).into_response();
        let Ok(value) =
            format!("{name}={cookie}; Path=/; HttpOnly; SameSite=Strict; Max-Age={LIFETIME}")
                .parse()
        else {
            return error(StatusCode::SERVICE_UNAVAILABLE);
        };
        response.headers_mut().insert(header::SET_COOKIE, value);
        return response;
    }
    if path == "/quick/logout" && method == Method::POST {
        if api
            .quick
            .authenticate(request.headers(), true)
            .await
            .is_none()
        {
            return error(StatusCode::UNAUTHORIZED);
        }
        let Some(cookie) = api.quick.cookie(request.headers()) else {
            return error(StatusCode::UNAUTHORIZED);
        };
        let mut registry = api.quick.registry.lock().await;
        let Some(index) = registry
            .browsers
            .iter()
            .position(|entry| equal(&entry.cookie, cookie))
        else {
            return error(StatusCode::UNAUTHORIZED);
        };
        if api
            .tokens
            .revoke(&registry.browsers[index].token_id)
            .await
            .is_err()
        {
            return error(StatusCode::SERVICE_UNAVAILABLE);
        }
        registry.browsers.swap_remove(index);
        let Some(name) = api.quick.cookie_name.get() else {
            return error(StatusCode::SERVICE_UNAVAILABLE);
        };
        let mut response = Json(serde_json::json!({})).into_response();
        let Ok(value) = format!("{name}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0").parse()
        else {
            return error(StatusCode::SERVICE_UNAVAILABLE);
        };
        response.headers_mut().insert(header::SET_COOKIE, value);
        return response;
    }
    if method != Method::GET || request.uri().query().is_some() {
        return error(StatusCode::NOT_FOUND);
    }
    let (kind, bytes): (&str, &[u8]) = match path.as_str() {
        "/quick/icon.png" => (
            "image/png",
            include_bytes!("../../../assets/images/brand/okytes_icon_transperant.png"),
        ),
        "/quick/logo.png" => (
            "image/png",
            include_bytes!("../../../assets/images/brand/okytes_logo_transperant.png"),
        ),
        _ => {
            let name = if path == "/quick" || path == "/quick/" {
                "quick.html"
            } else {
                path.strip_prefix("/quick/").unwrap_or("")
            };
            let Some((_, bytes)) = ASSETS.iter().find(|(key, _)| *key == name) else {
                return error(StatusCode::NOT_FOUND);
            };
            let kind = if name.ends_with(".html") {
                "text/html; charset=utf-8"
            } else if name.ends_with(".js") {
                "text/javascript; charset=utf-8"
            } else if name.ends_with(".css") {
                "text/css; charset=utf-8"
            } else {
                return error(StatusCode::NOT_FOUND);
            };
            (kind, bytes)
        }
    };
    ([(header::CONTENT_TYPE, kind)], bytes).into_response()
}

/// Browser capabilities are a strict subset of native control; only HTTP batches
/// and existing queue operations are admitted, never credential/config routes.
pub(crate) async fn browser_request(mut request: Request) -> Result<Request, Response> {
    let path = request.uri().path();
    let get = *request.method() == Method::GET
        && (path == "/v1/jobs"
            || path == "/v1/events"
            || (path.starts_with("/v1/jobs/") && !path.ends_with("/control")));
    let control = *request.method() == Method::POST
        && path.starts_with("/v1/jobs/")
        && path.ends_with("/control");
    let add = *request.method() == Method::POST && path == "/v1/jobs";
    if !get && !control && !add {
        return Err(error(StatusCode::FORBIDDEN));
    }
    if add {
        let (parts, stream) = request.into_parts();
        let bytes = body(Request::new(stream)).await?;
        let input: okytes_core::desktop::AddInput =
            serde_json::from_slice(&bytes).map_err(|_| error(StatusCode::BAD_REQUEST))?;
        if input.urls.is_empty()
            || input.urls.len() > 10
            || input.urls.iter().any(|value| {
                value.len() > 8192
                    || value.parse::<url::Url>().map_or(true, |url| {
                        !matches!(url.scheme(), "http" | "https")
                            || !url.username().is_empty()
                            || url.password().is_some()
                    })
            })
        {
            return Err(error(StatusCode::BAD_REQUEST));
        }
        request = Request::from_parts(parts, Body::from(bytes));
    }
    request.extensions_mut().insert(BrowserRequest);
    Ok(request)
}

pub(crate) fn secure(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    headers.insert(
        "x-content-type-options",
        header::HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", header::HeaderValue::from_static("DENY"));
    headers.insert(
        "referrer-policy",
        header::HeaderValue::from_static("no-referrer"),
    );
    headers.insert("content-security-policy", header::HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn expired_codes_and_sessions_are_denied_by_the_running_service_and_stream() {
        let root = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let api = Api::open(
            root.path(),
            &root.path().join("downloads"),
            crate::server::Policy {
                hosts: vec![address.to_string()],
                origins: vec![],
            },
        )
        .await
        .unwrap();
        api.quick.enable(&address.to_string()).unwrap();
        let router = crate::server::router(api.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let http = reqwest::Client::new();
        let url = format!("http://{address}");
        let old = api.quick.pair().await.unwrap();
        api.quick.registry.lock().await.codes[0].until = Instant::now() - Duration::from_secs(1);
        assert_eq!(
            http.post(format!("{url}/quick/session"))
                .header("x-okytes-quick", "1")
                .json(&serde_json::json!({"code":old.code}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let fresh = api.quick.pair().await.unwrap();
        let response = http
            .post(format!("{url}/quick/session"))
            .header("x-okytes-quick", "1")
            .json(&serde_json::json!({"code":fresh.code}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let csrf = response.json::<serde_json::Value>().await.unwrap()["csrf"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut stream = http
            .get(format!("{url}/v1/events"))
            .header("cookie", &cookie)
            .header("x-okytes-quick", "1")
            .header("x-okytes-csrf", &csrf)
            .send()
            .await
            .unwrap();
        let first = stream.chunk().await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&first).contains("Resync"));
        api.quick.registry.lock().await.browsers[0].until = Instant::now() - Duration::from_secs(1);
        assert_eq!(
            http.get(format!("{url}/quick/session"))
                .header("cookie", &cookie)
                .header("x-okytes-quick", "1")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            while stream.chunk().await.unwrap().is_some() {}
        })
        .await
        .unwrap();
        api.shutdown().await.unwrap();
        task.abort();
    }
}
