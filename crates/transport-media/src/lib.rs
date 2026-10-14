//! Validated VOD plans; durable data writes and publication remain in core.
mod manifests;
use bytes::Bytes;
use futures::StreamExt;
use okytes_core::{
    desktop::MediaChoice,
    error::TransportError,
    range::ByteRange,
    transport::{ContentRange, FetchResult, ProbeResult, Transport},
    types::RemoteIdentity,
};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
use url::Url;
type Future<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, TransportError>> + Send + 'a>>;
pub(crate) fn error(url: &Url, detail: impl std::fmt::Display) -> TransportError {
    TransportError::Configuration(format!("media {}: {detail}", url.path()))
}
const MAX_SEGMENT: usize = 32 * 1024 * 1024;
#[derive(Clone)]
pub(crate) struct Part {
    url: Url,
    range: Option<(u64, u64)>,
    identity: RemoteIdentity,
    length: u64,
    offset: u64,
    format: Format,
    key: Option<([u8; 16], [u8; 16])>,
    prepared: Option<Bytes>,
}
#[derive(Clone, Copy)]
pub(crate) enum Format {
    Ts,
    Mp4Init,
    Mp4,
    Vtt,
}
struct Plan {
    parts: Vec<Part>,
    identity: RemoteIdentity,
    extension: String,
}
type PlanCache = VecDeque<(String, Arc<Plan>)>;
/// Bounded media planner and HTTP dispatcher. Two segment buffers can be live,
/// each at most 32 MiB; at most 16 plans and 1024 segments per plan are retained.
pub struct MediaTransport {
    http: Arc<dyn Transport>,
    cache: Mutex<PlanCache>,
    buffers: Arc<Semaphore>,
}
impl MediaTransport {
    /// Compose media support with the existing validated HTTP transport.
    pub fn new(http: Arc<dyn Transport>) -> Arc<Self> {
        Arc::new(Self {
            http,
            cache: Mutex::new(VecDeque::new()),
            buffers: Arc::new(Semaphore::new(2)),
        })
    }
    /// Return explicit quality, language/audio and subtitle choices.
    pub async fn inspect(&self, source: &str) -> Result<Vec<MediaChoice>, TransportError> {
        let source = Url::parse(source)
            .map_err(|_| TransportError::Configuration("invalid media URL".into()))?;
        let (kind, url) = physical(&source)?;
        let (bytes, final_url) = self.document(&url, &[], 1024 * 1024).await?;
        manifests::choices(kind, &source, &final_url, &bytes)
    }
    pub(crate) async fn document(
        &self,
        url: &Url,
        headers: &[(String, String)],
        limit: usize,
    ) -> Result<(Bytes, Url), TransportError> {
        let probe = self.http.probe(url, headers).await?;
        if probe.content_length.is_some_and(|n| n > limit as u64) {
            return Err(error(url, "resource exceeds size bound"));
        }
        let data = self
            .read(
                &probe.final_url,
                headers,
                None,
                &probe.remote_identity,
                limit,
            )
            .await?;
        Ok((data, probe.final_url))
    }
    async fn read(
        &self,
        url: &Url,
        headers: &[(String, String)],
        range: Option<(u64, u64)>,
        identity: &RemoteIdentity,
        limit: usize,
    ) -> Result<Bytes, TransportError> {
        let response = self.http.fetch(url, headers, range, identity).await?;
        let mut body = response.body;
        let mut bytes = Vec::new();
        while let Some(chunk) = body.next().await {
            let chunk = chunk?;
            if chunk.len() > limit.saturating_sub(bytes.len()) {
                return Err(error(url, "segment exceeds bounded buffer"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let expected = range.map(|(a, b)| b - a + 1).or(response.content_length);
        if expected.is_some_and(|n| n != bytes.len() as u64) {
            return Err(error(url, "truncated or oversized segment"));
        }
        Ok(bytes.into())
    }
    async fn plan(
        &self,
        source: &Url,
        headers: &[(String, String)],
    ) -> Result<Arc<Plan>, TransportError> {
        let (kind, url) = physical(source)?;
        let (bytes, final_url) = self.document(&url, headers, 1024 * 1024).await?;
        let (mut parts, extension, documents) =
            manifests::resolve(self, kind, source, &final_url, &bytes, headers).await?;
        if parts.is_empty() || parts.len() > 1024 {
            return Err(error(source, "VOD must contain 1–1024 segments"));
        }
        let mut digest = Sha256::new();
        digest.update(source.as_str());
        digest.update(&bytes);
        for bytes in documents {
            digest.update(bytes);
        }
        let mut total = 0u64;
        let mut subtitle_bytes = 0usize;
        for (index, part) in parts.iter_mut().enumerate() {
            let probe = self
                .http
                .probe(&part.url, headers_for(&url, &part.url, headers))
                .await?;
            part.url = probe.final_url;
            part.identity = probe.remote_identity;
            if part.identity.strong_etag().is_none() && part.identity.last_modified.is_none() {
                return Err(error(
                    &part.url,
                    "segments require a strong ETag or Last-Modified for safe resume",
                ));
            }
            let size = probe
                .content_length
                .ok_or_else(|| error(&part.url, "segment Content-Length required"))?;
            if let Some((start, end)) = part.range {
                if start > end || end >= size {
                    return Err(error(&part.url, "invalid segment byte range"));
                }
            }
            let physical_length = part.range.map(|(a, b)| b - a + 1).unwrap_or(size);
            if physical_length == 0 || physical_length > MAX_SEGMENT as u64 {
                return Err(error(
                    &part.url,
                    "segment must be nonempty and at most 32 MiB",
                ));
            }
            part.length = physical_length;
            part.offset = total;
            if part.key.is_some() || matches!(part.format, Format::Vtt) {
                let _permit = self.buffers.acquire().await.map_err(|e| error(source, e))?;
                let data = self
                    .read(
                        &part.url,
                        headers_for(&url, &part.url, headers),
                        part.range,
                        &part.identity,
                        MAX_SEGMENT,
                    )
                    .await?;
                let data = decode(part, data, index == 0)?;
                part.length = data.len() as u64;
                // Subtitle normalization changes byte offsets. Keep the bounded
                // normalized text only; AES plaintext is re-read on each fetch.
                if matches!(part.format, Format::Vtt) {
                    subtitle_bytes += data.len();
                    if subtitle_bytes > 4 * 1024 * 1024 {
                        return Err(error(source, "subtitle track exceeds 4 MiB"));
                    }
                    part.prepared = Some(data);
                }
            }
            total = total
                .checked_add(part.length)
                .filter(|n| *n <= 64 * 1024 * 1024 * 1024)
                .ok_or_else(|| error(source, "media exceeds 64 GiB limit"))?;
            digest.update(part.url.as_str());
            digest.update(part.length.to_be_bytes());
            digest.update(format!("{:?}:{:?}", part.identity, part.range));
            if let Some((key, iv)) = part.key {
                digest.update(key);
                digest.update(iv);
            }
            if let Some(ref data) = part.prepared {
                digest.update(data);
            }
        }
        let identity = RemoteIdentity {
            etag: Some(format!("\"{:x}\"", digest.finalize())),
            last_modified: None,
            content_length: Some(total),
        };
        Ok(Arc::new(Plan {
            parts,
            identity,
            extension,
        }))
    }
    fn cached(
        &self,
        source: &Url,
        identity: &RemoteIdentity,
    ) -> Result<Option<Arc<Plan>>, TransportError> {
        Ok(self
            .cache
            .lock()
            .map_err(|_| error(source, "plan cache lock failed"))?
            .iter()
            .find(|(key, plan)| key == source.as_str() && plan.identity == *identity)
            .map(|(_, p)| p.clone()))
    }
}
pub(crate) fn headers_for<'a>(
    root: &Url,
    target: &Url,
    headers: &'a [(String, String)],
) -> &'a [(String, String)] {
    if root.origin() == target.origin() {
        headers
    } else {
        &[]
    }
}
pub(crate) fn physical(source: &Url) -> Result<(&str, Url), TransportError> {
    let (kind, scheme) = source.scheme().split_once('+').ok_or_else(|| {
        error(
            source,
            "use hls+https:// or dash+https:// (HTTP is also supported)",
        )
    })?;
    if !matches!(kind, "hls" | "dash")
        || !matches!(scheme, "http" | "https")
        || !source.username().is_empty()
        || source.password().is_some()
    {
        return Err(error(
            source,
            "unsupported media scheme or embedded credentials",
        ));
    }
    let mut url = Url::parse(&format!(
        "{scheme}:{}",
        &source.as_str()[source.scheme().len() + 1..]
    ))
    .map_err(|e| error(source, e))?;
    url.set_fragment(None);
    Ok((kind, url))
}
pub(crate) fn media_url(kind: &str, url: &Url, selection: Option<&str>) -> String {
    let mut result = format!("{kind}+{url}");
    if let Some(id) = selection {
        result.push('#');
        result.push_str(
            &url::form_urlencoded::Serializer::new(String::new())
                .append_pair("representation", id)
                .finish(),
        );
    }
    result
}
impl Transport for MediaTransport {
    fn supports(&self, scheme: &str) -> bool {
        matches!(
            scheme,
            "hls+http" | "hls+https" | "dash+http" | "dash+https"
        ) || self.http.supports(scheme)
    }
    fn probe(&self, url: &Url, headers: &[(String, String)]) -> Future<'_, ProbeResult> {
        if !url.scheme().contains('+') {
            return self.http.probe(url, headers);
        }
        let source = url.clone();
        let headers = headers.to_vec();
        Box::pin(async move {
            let plan = self.plan(&source, &headers).await?;
            let result = ProbeResult {
                checkpoint_ranges: Some(
                    plan.parts
                        .iter()
                        .map(|p| ByteRange {
                            start: p.offset,
                            end: p.offset + p.length - 1,
                        })
                        .collect(),
                ),
                final_url: source.clone(),
                content_length: plan.identity.content_length,
                accepts_ranges: true,
                remote_identity: plan.identity.clone(),
                suggested_filename: Some(format!(
                    "{}.{}",
                    okytes_core::engine::sanitize(
                        source
                            .path()
                            .rsplit('/')
                            .next()
                            .unwrap_or("media")
                            .rsplit_once('.')
                            .map(|(name, _)| name)
                            .unwrap_or("media")
                    ),
                    plan.extension
                )),
                content_type: None,
            };
            let mut cache = self
                .cache
                .lock()
                .map_err(|_| error(&source, "plan cache lock failed"))?;
            cache
                .retain(|(key, cached)| key != source.as_str() || cached.identity != plan.identity);
            while cache.len() >= 16 {
                cache.pop_front();
            }
            cache.push_back((source.to_string(), plan));
            Ok(result)
        })
    }
    fn fetch(
        &self,
        url: &Url,
        headers: &[(String, String)],
        range: Option<(u64, u64)>,
        identity: &RemoteIdentity,
    ) -> Future<'_, FetchResult> {
        if !url.scheme().contains('+') {
            return self.http.fetch(url, headers, range, identity);
        }
        let source = url.clone();
        let headers = headers.to_vec();
        let identity = identity.clone();
        let http = self.http.clone();
        let buffers = self.buffers.clone();
        Box::pin(async move {
            // A plan is session-local and is reconstructed after process loss.
            let plan = match self.cached(&source, &identity)? {
                Some(plan) => plan,
                None => self.plan(&source, &headers).await?,
            };
            if !identity.is_compatible(&plan.identity) {
                return Err(error(
                    &source,
                    "manifest, selected segment identity or AES key changed; restart required",
                ));
            }
            let total = plan.identity.content_length.unwrap_or(0);
            let (start, end) = range.unwrap_or((0, total.saturating_sub(1)));
            if start > end || end >= total {
                return Err(error(&source, "invalid assembled byte range"));
            }
            let (_, root) = physical(&source)?;
            let stream_source = source.clone();
            let body = async_stream::try_stream! {
                let _permit=buffers.acquire_owned().await.map_err(|e|error(&stream_source,e))?;
                for (index,part) in plan.parts.iter().enumerate(){
                    if part.offset+part.length<=start||part.offset>end{continue;}
                    let data=if let Some(ref prepared)=part.prepared{prepared.clone()}else{
                        let response=http.fetch(&part.url,headers_for(&root,&part.url,&headers),part.range,&part.identity).await?;let mut stream=response.body;let mut bytes=Vec::new();
                        while let Some(chunk)=stream.next().await{let chunk=chunk?;if chunk.len()>MAX_SEGMENT.saturating_sub(bytes.len()){Err(error(&part.url,"segment exceeds bounded buffer"))?;}bytes.extend_from_slice(&chunk);}
                        let size=part.range.map(|(a,b)|b-a+1).or(part.identity.content_length);
                        if size!=Some(bytes.len() as u64){Err(error(&part.url,"truncated media segment"))?;}
                        decode(part,bytes.into(),index==0)?
                    };
                    if data.len() as u64!=part.length{Err(error(&part.url,"decoded segment length changed"))?;}
                    let a=start.saturating_sub(part.offset) as usize;let b=((end-part.offset+1).min(part.length)) as usize;
                    for chunk in data.slice(a..b).chunks(32768){yield Bytes::copy_from_slice(chunk);}
                }
            };
            Ok(FetchResult {
                status: if range.is_some() { 206 } else { 200 },
                content_length: Some(end - start + 1),
                content_range: range.map(|_| ContentRange {
                    start,
                    end,
                    total: Some(total),
                }),
                remote_identity: identity,
                body: Box::pin(body),
            })
        })
    }
}
fn decode(part: &Part, bytes: Bytes, first: bool) -> Result<Bytes, TransportError> {
    use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    let bytes = if let Some((key, iv)) = part.key {
        cbc::Decryptor::<aes::Aes128>::new(&key.into(), &iv.into())
            .decrypt_padded_vec_mut::<Pkcs7>(&bytes)
            .map(Bytes::from)
            .map_err(|_| error(&part.url, "invalid AES-128 ciphertext or padding"))?
    } else {
        bytes
    };
    if bytes.is_empty() {
        return Err(error(&part.url, "empty decoded media segment"));
    }
    match part.format {
        Format::Ts => {
            if bytes.len() % 188 != 0
                || bytes
                    .chunks(188)
                    .any(|packet| packet.first() != Some(&0x47))
            {
                return Err(error(&part.url, "invalid MPEG-TS packet framing"));
            }
        }
        Format::Mp4Init | Format::Mp4 => {
            let kinds = mp4_boxes(&bytes, &part.url, 0)?;
            let required = if matches!(part.format, Format::Mp4Init) {
                b"moov"
            } else {
                b"moof"
            };
            if !kinds.contains(&required.as_slice())
                || matches!(part.format, Format::Mp4) && !kinds.contains(&b"mdat".as_slice())
            {
                return Err(error(
                    &part.url,
                    "fragmented MP4 requires moov init and moof/mdat media",
                ));
            }
        }
        Format::Vtt => {
            let text = std::str::from_utf8(&bytes)
                .map_err(|e| error(&part.url, e))?
                .trim_start_matches('\u{feff}')
                .replace("\r\n", "\n");
            if !text.starts_with("WEBVTT")
                || text.contains("X-TIMESTAMP-MAP")
                || text.contains("STYLE")
                || text.contains("REGION")
            {
                return Err(error(&part.url,"only UTF-8 WebVTT with absolute timestamps and no timestamp-map/style/region blocks is supported"));
            }
            let (_, cues) = text
                .split_once("\n\n")
                .ok_or_else(|| error(&part.url, "WebVTT header separator required"))?;
            for block in cues.split("\n\n").filter(|block| !block.trim().is_empty()) {
                let mut lines = block.lines();
                let first = lines.next().unwrap_or("");
                if first.starts_with("NOTE") {
                    continue;
                }
                let timing = if first.contains("-->") {
                    first
                } else {
                    lines.next().unwrap_or("")
                };
                let (start, end) = timing
                    .split_once("-->")
                    .ok_or_else(|| error(&part.url, "invalid WebVTT cue timing"))?;
                let start = vtt_time(start.trim())
                    .ok_or_else(|| error(&part.url, "invalid WebVTT timestamp"))?;
                let end = vtt_time(end.split_whitespace().next().unwrap_or(""))
                    .ok_or_else(|| error(&part.url, "invalid WebVTT timestamp"))?;
                if end <= start {
                    return Err(error(&part.url, "WebVTT cue end must follow its start"));
                }
            }
            return Ok(Bytes::from(format!(
                "{}{}\n\n",
                if first { "WEBVTT\n\n" } else { "" },
                cues.trim_end()
            )));
        }
    }
    Ok(bytes)
}
fn vtt_time(value: &str) -> Option<u64> {
    let fields = value.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&fields.len()) {
        return None;
    }
    let (seconds, milliseconds) = fields.last()?.split_once('.')?;
    if milliseconds.len() != 3 || !milliseconds.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let seconds = seconds.parse::<u64>().ok()?;
    let minutes = fields[fields.len() - 2].parse::<u64>().ok()?;
    if seconds >= 60 || minutes >= 60 {
        return None;
    }
    let hours = if fields.len() == 3 {
        fields[0].parse::<u64>().ok()?
    } else {
        0
    };
    hours
        .checked_mul(3600000)?
        .checked_add(minutes * 60000 + seconds * 1000 + milliseconds.parse::<u64>().ok()?)
}

fn mp4_boxes<'a>(bytes: &'a [u8], url: &Url, depth: u8) -> Result<Vec<&'a [u8]>, TransportError> {
    if depth > 16 {
        return Err(error(url, "MP4 nesting exceeds 16"));
    }
    let mut offset = 0;
    let mut kinds = Vec::new();
    while offset < bytes.len() {
        if bytes.len() - offset < 8 {
            return Err(error(url, "truncated MP4 box"));
        }
        let size = u32::from_be_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .map_err(|e| error(url, e))?,
        ) as usize;
        if size < 8 || size > bytes.len() - offset {
            return Err(error(url, "unsupported or corrupt MP4 box size"));
        }
        let kind = &bytes[offset + 4..offset + 8];
        let payload = &bytes[offset + 8..offset + size];
        if matches!(
            kind,
            b"pssh" | b"sinf" | b"tenc" | b"encv" | b"enca" | b"senc"
        ) {
            return Err(error(url, "encrypted/DRM-signalled MP4 is refused"));
        }
        if matches!(
            kind,
            b"moov"
                | b"trak"
                | b"mdia"
                | b"minf"
                | b"stbl"
                | b"mvex"
                | b"moof"
                | b"traf"
                | b"edts"
                | b"dinf"
        ) {
            mp4_boxes(payload, url, depth + 1)?;
        } else if kind == b"stsd" {
            if payload.len() < 8 {
                return Err(error(url, "truncated sample description"));
            }
            mp4_boxes(&payload[8..], url, depth + 1)?;
        }
        kinds.push(kind);
        offset += size;
    }
    Ok(kinds)
}
