//! Website link crawler. Breadth-first over HTML pages, bounded in depth,
//! pages, links and bytes, honouring robots.txt. It only collects candidate
//! file links for review; it never starts downloads by itself.
use crate::{
    desktop::{CrawlInput, CrawlLink, CrawlResult},
    error::{EngineError, EngineResult},
    transport::Transport,
    types::RemoteIdentity,
};
use futures::StreamExt;
use std::collections::{HashSet, VecDeque};
use url::Url;

const MAX_PAGE: usize = 2 * 1024 * 1024;
const MAX_LINKS: usize = 2000;
const PAGE_TYPES: &[&str] = &["", "htm", "html", "php", "asp", "aspx", "jsp", "shtml"];

fn invalid(message: &str) -> EngineError {
    EngineError::Config(format!("crawl: {message}"))
}

fn extension(url: &Url) -> String {
    url.path_segments()
        .and_then(|mut s| s.next_back())
        .and_then(|name| {
            name.rsplit_once('.')
                .map(|(_, ext)| ext.to_ascii_lowercase())
        })
        .unwrap_or_default()
}

/// Collect href/src attribute values. A scanner, not a parser: enough for
/// links in real pages, and it never executes or interprets markup.
pub fn attribute_links(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    for attribute in ["href", "src"] {
        let mut from = 0;
        while let Some(at) = lower[from..].find(attribute) {
            let start = from + at + attribute.len();
            from = start;
            // Must be a whole attribute name followed by optional spaces and '='.
            if lower[..from - attribute.len()]
                .chars()
                .last()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
            {
                continue;
            }
            let rest = html[start..].trim_start();
            let Some(rest) = rest.strip_prefix('=') else {
                continue;
            };
            let rest = rest.trim_start();
            let value = match rest.chars().next() {
                Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or_default(),
                Some(_) => rest
                    .split(|c: char| c.is_whitespace() || c == '>')
                    .next()
                    .unwrap_or_default(),
                None => "",
            };
            if !value.is_empty() && value.len() <= 8192 {
                found.push(value.replace("&amp;", "&"));
            }
        }
    }
    found
}

/// `Disallow` prefixes for user-agent `*` (the crawler identifies generically).
fn robots_rules(text: &str) -> Vec<String> {
    let mut rules = Vec::new();
    let mut applies = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
        match key.as_str() {
            "user-agent" => applies = value == "*",
            "disallow" if applies && !value.is_empty() => rules.push(value.to_owned()),
            _ => {}
        }
    }
    rules
}

async fn get_text(transport: &dyn Transport, url: &Url) -> Option<String> {
    let response = transport
        .fetch(url, &[], None, &RemoteIdentity::empty())
        .await
        .ok()?;
    let mut body = response.body;
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.ok()?;
        if bytes.len() + chunk.len() > MAX_PAGE {
            // Oversized pages are scanned up to the limit only.
            bytes.extend_from_slice(&chunk[..MAX_PAGE - bytes.len()]);
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

pub async fn crawl(transport: &dyn Transport, input: CrawlInput) -> EngineResult<CrawlResult> {
    let start =
        Url::parse(input.url.trim()).map_err(|_| invalid("enter a full http(s) address"))?;
    if !matches!(start.scheme(), "http" | "https")
        || !start.username().is_empty()
        || start.password().is_some()
    {
        return Err(invalid(
            "enter an http(s) address without a username or password",
        ));
    }
    if input.depth > 3 || !(1..=200).contains(&input.max_pages) || input.extensions.len() > 64 {
        return Err(invalid("use depth 0–3 and 1–200 pages"));
    }
    let wanted: HashSet<String> = input
        .extensions
        .iter()
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    let host = start.host_str().unwrap_or_default().to_owned();
    let mut robots = Url::parse(&start.origin().ascii_serialization())
        .ok()
        .and_then(|origin| origin.join("/robots.txt").ok());
    let disallowed = match robots.take() {
        Some(url) => robots_rules(&get_text(transport, &url).await.unwrap_or_default()),
        None => vec![],
    };
    let blocked = |url: &Url| {
        url.host_str() == Some(host.as_str())
            && disallowed
                .iter()
                .any(|rule| url.path().starts_with(rule.as_str()))
    };
    let mut queue = VecDeque::from([(start.clone(), 0u32)]);
    let mut visited = HashSet::new();
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    let (mut pages, mut skipped, mut truncated) = (0u32, 0u32, false);
    while let Some((page, depth)) = queue.pop_front() {
        if !visited.insert(page.clone()) {
            continue;
        }
        if blocked(&page) {
            skipped += 1;
            continue;
        }
        if pages >= input.max_pages {
            truncated = true;
            break;
        }
        pages += 1;
        let Some(html) = get_text(transport, &page).await else {
            continue;
        };
        for raw in attribute_links(&html) {
            let Ok(mut url) = page.join(&raw) else {
                continue;
            };
            url.set_fragment(None);
            if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() {
                continue;
            }
            let same_host = url.host_str() == Some(host.as_str());
            let ext = extension(&url);
            if PAGE_TYPES.contains(&ext.as_str()) {
                if same_host && depth < input.depth && !visited.contains(&url) {
                    queue.push_back((url, depth + 1));
                }
                continue;
            }
            if (input.same_host && !same_host)
                || (!wanted.is_empty() && !wanted.contains(&ext))
                || !seen.insert(url.clone())
            {
                continue;
            }
            if links.len() >= MAX_LINKS {
                truncated = true;
                break;
            }
            links.push(CrawlLink {
                url: url.into(),
                page: page.to_string(),
                extension: ext,
            });
        }
        // Be a polite client: one page at a time with a short pause.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    Ok(CrawlResult {
        links,
        pages,
        skipped_by_robots: skipped,
        truncated,
    })
}
