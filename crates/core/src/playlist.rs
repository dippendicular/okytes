//! Plain playlist expansion: M3U/M3U8 lists, PLS and XSPF. Returns the listed
//! links for review; HLS streaming manifests are refused here because the
//! media inspector handles them with track selection.
use crate::{
    error::{EngineError, EngineResult},
    transport::Transport,
    types::RemoteIdentity,
};
use futures::StreamExt;
use url::Url;

const MAX_BYTES: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 1000;

fn invalid(message: &str) -> EngineError {
    EngineError::Config(format!("playlist: {message}"))
}

/// Parse playlist text against its own URL. Pure, so it is easy to reason about.
pub fn entries(text: &str, base: &Url) -> EngineResult<Vec<String>> {
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if trimmed.contains("#EXT-X-TARGETDURATION") || trimmed.contains("#EXT-X-STREAM-INF") {
        return Err(invalid(
            "this is an HLS stream, not a list of files; use Add media to choose its tracks",
        ));
    }
    let raw: Vec<String> = if trimmed.starts_with('<') {
        // XSPF: <location>…</location> elements.
        trimmed
            .split("<location>")
            .skip(1)
            .filter_map(|part| part.split("</location>").next())
            .map(|s| s.trim().replace("&amp;", "&"))
            .collect()
    } else if trimmed.to_ascii_lowercase().starts_with("[playlist]") {
        trimmed
            .lines()
            .filter_map(|line| line.trim().split_once('='))
            .filter(|(key, _)| key.to_ascii_lowercase().starts_with("file"))
            .map(|(_, value)| value.trim().to_owned())
            .collect()
    } else {
        trimmed
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_owned)
            .collect()
    };
    let mut links = Vec::new();
    for item in raw {
        // Local paths in a remote playlist cannot be downloaded; skip them.
        let Ok(url) = base.join(&item) else { continue };
        if matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && !links.contains(&url.to_string())
        {
            links.push(url.to_string());
        }
        if links.len() >= MAX_ENTRIES {
            break;
        }
    }
    if links.is_empty() {
        return Err(invalid("no downloadable http(s) entries were found"));
    }
    Ok(links)
}

/// Fetch a playlist (bounded to 1 MiB) and expand it.
pub async fn expand(transport: &dyn Transport, source: &str) -> EngineResult<Vec<String>> {
    let url = Url::parse(source.trim()).map_err(|_| invalid("enter a full http(s) address"))?;
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() {
        return Err(invalid("enter an http(s) address without a username"));
    }
    let response = transport
        .fetch(&url, &[], None, &RemoteIdentity::empty())
        .await
        .map_err(|e| invalid(&e.to_string()))?;
    let mut body = response.body;
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| invalid(&e.to_string()))?;
        if bytes.len() + chunk.len() > MAX_BYTES {
            return Err(invalid("playlist is larger than 1 MiB"));
        }
        bytes.extend_from_slice(&chunk);
    }
    entries(&String::from_utf8_lossy(&bytes), &url)
}
