//! HTTP transport with certificate validation, identity encoding, strict range
//! validation, redirect-aware origin limits, and no decompression of file bytes.

use futures::StreamExt;
use okytes_core::{
    error::TransportError,
    transport::{ContentRange, FetchResult, ProbeResult, Transport},
    types::{EngineConfig, RemoteIdentity},
};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue},
    Client, Method, Response,
};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use url::Url;

/// Rustls HTTP client shared across all jobs; permits live until bodies drop.
pub struct HttpTransport {
    client: Client,
    origins: Mutex<HashMap<String, Weak<Semaphore>>>,
    cap: usize,
    global: Arc<Semaphore>,
}
struct ConnectionPermit {
    _origin: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
}

fn request_error(url: &Url, error: impl std::fmt::Display) -> TransportError {
    TransportError::Request {
        url: Box::new(url.clone()),
        detail: error.to_string(),
    }
}

impl HttpTransport {
    /// Build a client with verified TLS and bounded per-origin connections.
    pub fn new(config: &EngineConfig) -> Result<Self, TransportError> {
        config
            .validate()
            .map_err(|e| TransportError::Configuration(e.to_string()))?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
            .read_timeout(Duration::from_secs(config.read_timeout_secs))
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|e| TransportError::Configuration(e.to_string()))?;
        Ok(Self {
            client,
            origins: Mutex::new(HashMap::new()),
            cap: config.max_connections_per_origin.max(1),
            global: Arc::new(Semaphore::new(config.max_connections_total)),
        })
    }

    async fn send(
        &self,
        method: Method,
        url: &Url,
        headers: &[(String, String)],
        range: Option<(u64, u64)>,
        identity: Option<&RemoteIdentity>,
    ) -> Result<(Response, ConnectionPermit), TransportError> {
        let mut target = url.clone();
        for _ in 0..=10 {
            if !matches!(target.scheme(), "http" | "https")
                || !target.username().is_empty()
                || target.password().is_some()
            {
                return Err(request_error(
                    &target,
                    "only HTTP(S) URLs without embedded credentials are allowed",
                ));
            }
            let origin = target.origin().ascii_serialization();
            let semaphore = {
                let mut origins = self.origins.lock().map_err(|e| request_error(&target, e))?;
                origins.retain(|_, value| value.strong_count() > 0);
                let semaphore = origins
                    .get(&origin)
                    .and_then(Weak::upgrade)
                    .unwrap_or_else(|| Arc::new(Semaphore::new(self.cap)));
                origins.insert(origin, Arc::downgrade(&semaphore));
                semaphore
            };
            let global = self
                .global
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| request_error(&target, e))?;
            let origin_permit = semaphore
                .acquire_owned()
                .await
                .map_err(|e| request_error(&target, e))?;
            let permit = ConnectionPermit {
                _origin: origin_permit,
                _global: global,
            };
            let mut map = HeaderMap::new();
            for (name, value) in headers {
                let name = HeaderName::from_bytes(name.as_bytes())
                    .map_err(|e| request_error(&target, e))?;
                if matches!(
                    name.as_str(),
                    "range" | "if-range" | "accept-encoding" | "host" | "content-length"
                ) {
                    return Err(request_error(&target, "reserved download header"));
                }
                if target.origin() != url.origin() {
                    continue;
                }
                map.insert(
                    name,
                    HeaderValue::from_str(value).map_err(|e| request_error(&target, e))?,
                );
            }
            map.insert("accept-encoding", HeaderValue::from_static("identity"));
            if let Some((start, end)) = range {
                map.insert(
                    "range",
                    HeaderValue::from_str(&format!("bytes={start}-{end}"))
                        .map_err(|e| request_error(&target, e))?,
                );
                if let Some(etag) = identity.and_then(RemoteIdentity::strong_etag) {
                    map.insert(
                        "if-range",
                        HeaderValue::from_str(etag).map_err(|e| request_error(&target, e))?,
                    );
                }
            }
            let response = self
                .client
                .request(method.clone(), target.clone())
                .headers(map)
                .send()
                .await
                .map_err(|e| request_error(&target, e))?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|h| h.to_str().ok())
                    .ok_or_else(|| request_error(&target, "redirect missing Location"))?;
                let next = target
                    .join(location)
                    .map_err(|e| request_error(&target, e))?;
                if target.scheme() == "https" && next.scheme() != "https" {
                    return Err(request_error(&target, "HTTPS downgrade refused"));
                }
                target = next;
                continue;
            }
            return Ok((response, permit));
        }
        Err(TransportError::TooManyRedirects {
            url: Box::new(url.clone()),
        })
    }
}

fn text_header(
    headers: &HeaderMap,
    name: &str,
    url: &Url,
) -> Result<Option<String>, TransportError> {
    headers
        .get(name)
        .map(|h| {
            h.to_str()
                .map(str::to_owned)
                .map_err(|e| request_error(url, e))
        })
        .transpose()
}

fn identity(response: &Response, total: Option<u64>) -> Result<RemoteIdentity, TransportError> {
    let etag =
        text_header(response.headers(), "etag", response.url())?.filter(|s| !s.starts_with("W/"));
    Ok(RemoteIdentity {
        etag,
        last_modified: text_header(response.headers(), "last-modified", response.url())?,
        content_length: total,
    })
}

fn status(response: &Response) -> Result<(), TransportError> {
    let url = Box::new(response.url().clone());
    let code = response.status().as_u16();
    match code {
        200 | 206 => Ok(()),
        429 | 503 => {
            let value = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok());
            let retry_after_secs = value.and_then(|v| {
                v.parse().ok().or_else(|| {
                    chrono::DateTime::parse_from_rfc2822(v).ok().map(|date| {
                        (date.timestamp() - chrono::Utc::now().timestamp()).max(0) as u64
                    })
                })
            });
            Err(TransportError::Throttled {
                url,
                retry_after_secs,
            })
        }
        401 | 403 | 410 => Err(TransportError::UrlExpired { url }),
        _ => Err(TransportError::UnexpectedStatus {
            url,
            status: code,
            detail: "resource request rejected".into(),
        }),
    }
}

fn parse_range(value: &str) -> Option<ContentRange> {
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start: u64 = start.parse().ok()?;
    let end: u64 = end.parse().ok()?;
    let total: u64 = total.parse().ok()?;
    (start <= end && end < total).then_some(ContentRange {
        start,
        end,
        total: Some(total),
    })
}

impl Transport for HttpTransport {
    fn probe(
        &self,
        url: &Url,
        headers: &[(String, String)],
    ) -> Pin<Box<dyn Future<Output = Result<ProbeResult, TransportError>> + Send + '_>> {
        let url = url.clone();
        let headers = headers.to_vec();
        Box::pin(async move {
            let (mut response, mut permit) =
                self.send(Method::HEAD, &url, &headers, None, None).await?;
            if matches!(response.status().as_u16(), 405 | 501) {
                drop(response);
                drop(permit);
                (response, permit) = self.send(Method::GET, &url, &headers, None, None).await?;
            }
            status(&response)?;
            let total = text_header(response.headers(), "content-length", response.url())?
                .map(|s| {
                    s.parse::<u64>()
                        .map_err(|e| request_error(response.url(), e))
                })
                .transpose()?;
            let remote_identity = identity(&response, total)?;
            let accepts_ranges = response
                .headers()
                .get("accept-ranges")
                .is_some_and(|h| h == "bytes")
                && remote_identity.strong_etag().is_some();
            let result = ProbeResult {
                final_url: response.url().clone(),
                content_length: total,
                accepts_ranges,
                remote_identity,
                suggested_filename: text_header(
                    response.headers(),
                    "content-disposition",
                    response.url(),
                )?
                .and_then(|s| {
                    s.split(';')
                        .filter_map(|part| part.trim().split_once('='))
                        .find(|(name, _)| name.eq_ignore_ascii_case("filename"))
                        .map(|(_, value)| value.trim().trim_matches('"').to_owned())
                }),
                content_type: text_header(response.headers(), "content-type", response.url())?,
            };
            drop(permit);
            Ok(result)
        })
    }
    fn fetch(
        &self,
        url: &Url,
        headers: &[(String, String)],
        range: Option<(u64, u64)>,
        expected: &RemoteIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<FetchResult, TransportError>> + Send + '_>> {
        let url = url.clone();
        let headers = headers.to_vec();
        let expected = expected.clone();
        Box::pin(async move {
            let (response, permit) = self
                .send(Method::GET, &url, &headers, range, Some(&expected))
                .await?;
            status(&response)?;
            let code = response.status().as_u16();
            let content_length = response.content_length();
            let content_range = if code == 206 {
                Some(
                    text_header(response.headers(), "content-range", &url)?
                        .and_then(|s| parse_range(&s))
                        .ok_or_else(|| TransportError::InvalidContentRange {
                            url: Box::new(url.clone()),
                            detail: "missing or invalid byte range".into(),
                        })?,
                )
            } else {
                None
            };
            let observed = identity(
                &response,
                content_range.and_then(|r| r.total).or(content_length),
            )?;
            if !expected.is_compatible(&observed) {
                return Err(request_error(
                    &url,
                    "remote identity changed or validators disappeared",
                ));
            }
            if let Some((start, end)) = range {
                if code == 200 {
                    return Err(TransportError::RangeIgnored { url: Box::new(url) });
                }
                if content_range
                    != Some(ContentRange {
                        start,
                        end,
                        total: expected.content_length,
                    })
                    || content_length.is_some_and(|n| n != end - start + 1)
                {
                    return Err(TransportError::WrongRange {
                        url: Box::new(url),
                        expected: format!("{start}-{end}"),
                        actual: format!("{content_range:?}"),
                    });
                }
            } else if code != 200 {
                return Err(request_error(&url, "unsolicited partial response"));
            }
            if response
                .headers()
                .get("content-encoding")
                .is_some_and(|h| h != "identity")
            {
                return Err(request_error(&url, "encoded representation refused"));
            }
            let body = futures::stream::unfold(
                (response.bytes_stream(), permit, 0u64, false),
                move |(mut stream, permit, count, done)| {
                    let url = url.clone();
                    async move {
                        if done {
                            return None;
                        }
                        match stream.next().await {
                            Some(Ok(bytes)) => {
                                let count = count.saturating_add(bytes.len() as u64);
                                if content_length.is_some_and(|n| count > n) {
                                    Some((
                                        Err(request_error(&url, "body exceeds Content-Length")),
                                        (stream, permit, count, true),
                                    ))
                                } else {
                                    Some((Ok(bytes), (stream, permit, count, false)))
                                }
                            }
                            Some(Err(error)) => Some((
                                Err(request_error(&url, error)),
                                (stream, permit, count, true),
                            )),
                            None if content_length.is_some_and(|n| n != count) => Some((
                                Err(request_error(&url, "truncated body")),
                                (stream, permit, count, true),
                            )),
                            None => None,
                        }
                    }
                },
            );
            Ok(FetchResult {
                status: code,
                content_length,
                content_range,
                remote_identity: observed,
                body: Box::pin(body),
            })
        })
    }
}
